//! Exact idle visit writes, ordered with the saver's ordinary rewards and snapshots.
//!
//! The host has already checked the authored story, resource and scratch effects. This
//! layer carries their complete immutable before/after images to storage; it does not run
//! initializers, merge scratch, infer an entry, or admit a visit to the field runtime.

use std::collections::BTreeSet;

use fmv_adapter_api::GameAdapter;
use fmv_core::{AccountId, GameId, MAX_INVENTORY_STACKS};
use fmv_persist::{FieldVisitCommitted, FieldVisitOperation, FieldVisitOperationKind, FieldVisitV1,
    PersistError, SceneState, Store, MAX_FIELD_VISIT_ANCHOR_BYTES, MAX_FIELD_VISIT_BYTES,
    MAX_FIELD_VISIT_ENTRY_BYTES, MAX_FIELD_VISIT_FIELD_BYTES, SCHEMA_VERSION};
use fmv_protocol::MAX_STORY_PROGRESS_BYTES;

use super::SceneSaveError;

/// A complete accepted visit decision. Storage binds the operation to these exact images,
/// including their timestamps and caller schemas. Keep this job for an exact retry; do not
/// construct another afterimage from a later clock or overwrite a returned current state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FieldVisitSaveJob {
    operation: FieldVisitOperation,
    expected: SceneState,
    next: SceneState,
}

impl FieldVisitSaveJob {
    /// Freeze the transaction, stamping its two afterimage records exactly once. The caller
    /// must obtain `expected` through an ordered snapshot while holding the account against
    /// dependent gameplay; storage still compares the entire predecessor at commit time.
    pub(crate) fn new(operation: FieldVisitOperation, expected: SceneState, mut next: SceneState,
        now: u64) -> Result<Self, SceneSaveError> {
        next.pair.story.updated_at = now;
        next.pair.character.as_mut().ok_or(SceneSaveError::MissingCharacter)?.updated_at = now;
        let job = Self { operation, expected, next };
        job.validate()?;
        Ok(job)
    }

    pub(crate) fn operation(&self) -> FieldVisitOperation { self.operation }
    pub(crate) fn expected(&self) -> &SceneState { &self.expected }
    pub(crate) fn next(&self) -> &SceneState { &self.next }

    pub(super) fn account(&self) -> AccountId { self.expected.pair.story.account_id }

    /// Bound both images before they enter the unbounded FIFO. The adapter and Store repeat
    /// their own semantic and exact-state checks on the blocking worker before any mutation.
    pub(super) fn validate(&self) -> Result<(), SceneSaveError> {
        if self.operation.id == [0; 16] { return Err(SceneSaveError::InvalidRequest); }
        let game = self.expected.pair.story.game;
        for state in [&self.expected, &self.next] {
            if state.pair.story.account_id != self.account() || state.pair.story.game != game
                || state.pair.story.schema_version != SCHEMA_VERSION
                || state.pair.story.blob.len() > MAX_STORY_PROGRESS_BYTES
                || state.checkpoint.is_some() || state.reservation.is_some() {
                return Err(SceneSaveError::InvalidRequest);
            }
            let character = state.pair.character.as_ref().ok_or(SceneSaveError::MissingCharacter)?;
            if character.account_id != self.account() || character.game != game
                || !(1..=4).contains(&character.schema_version) || character.level == 0 {
                return Err(SceneSaveError::InvalidRequest);
            }
            if let Some(inventory) = &character.inventory {
                if inventory.len() > MAX_INVENTORY_STACKS { return Err(SceneSaveError::InvalidRequest); }
                let mut seen = BTreeSet::new();
                if inventory.iter().any(|item| item.count == 0 || !seen.insert(item.item)) {
                    return Err(SceneSaveError::InvalidRequest);
                }
            }
            if let Some(visit) = &state.field_visit { validate_visit(visit)?; }
        }
        let next = self.next.field_visit.as_ref().ok_or(SceneSaveError::InvalidRequest)?;
        match self.operation.kind {
            FieldVisitOperationKind::Enter => {
                if next.revision != 0 || next.entry.operation != self.operation.id
                    || self.expected.field_visit.as_ref().is_some_and(|old| old.visit_id == next.visit_id) {
                    return Err(SceneSaveError::InvalidRequest);
                }
            }
            FieldVisitOperationKind::Advance => {
                let old = self.expected.field_visit.as_ref().ok_or(SceneSaveError::InvalidRequest)?;
                if old.revision.checked_add(1) != Some(next.revision) || old.visit_id != next.visit_id
                    || old.field != next.field || old.source_identity != next.source_identity || old.entry != next.entry
                    || old.scratch_known_masks.iter().zip(&next.scratch_known_masks).any(|(old, next)| old & !next != 0) {
                    return Err(SceneSaveError::InvalidRequest);
                }
            }
        }
        Ok(())
    }
}

pub(crate) fn validate_visit(visit: &FieldVisitV1) -> Result<(), SceneSaveError> {
    if visit.schema_version != 1 || visit.visit_id == [0; 16] || visit.source_identity == [0; 32]
        || visit.entry.operation == [0; 16] || visit.active_continuation.is_some()
        || visit.field.is_empty() || visit.field.len() > MAX_FIELD_VISIT_FIELD_BYTES
        || visit.field.chars().any(char::is_control)
        || visit.entry.arrival.is_empty() || visit.entry.arrival.len() > MAX_FIELD_VISIT_ENTRY_BYTES
        || visit.anchor.is_empty() || visit.anchor.len() > MAX_FIELD_VISIT_ANCHOR_BYTES
        || visit.scratch_values.iter().zip(&visit.scratch_known_masks).any(|(value, mask)| value & !mask != 0)
        || !serde_json::to_vec(visit).is_ok_and(|bytes| bytes.len() <= MAX_FIELD_VISIT_BYTES) {
        return Err(SceneSaveError::InvalidRequest);
    }
    Ok(())
}

/// Blocking, single-attempt commit. In particular, do not read/compare the current state
/// before the Store checks its receipt: an exact duplicate may legitimately follow rewards.
/// Only a full-state conflict is a stale refusal; uncertain I/O and recovery errors retain
/// their fatal Store error so the caller cannot publish success or retry without recovery.
pub(crate) fn persist_field_visit(store: &Store, game: GameId, adapter: &dyn GameAdapter,
    job: &FieldVisitSaveJob) -> Result<FieldVisitCommitted, SceneSaveError> {
    job.validate()?;
    let expected = &job.expected().pair.story;
    let next = &job.next().pair.story;
    if game != expected.game || adapter.game() != game
        || adapter.compare_exchange_progression(&expected.blob, &expected.blob, &next.blob).as_deref() != Some(next.blob.as_slice()) {
        return Err(SceneSaveError::InvalidRequest);
    }
    store.commit_field_visit(&job.operation(), job.expected(), job.next()).map_err(|error| match error {
        PersistError::TransactionConflict(_) => SceneSaveError::StaleState,
        error => SceneSaveError::Store(error),
    })
}

#[cfg(test)]
mod tests;
