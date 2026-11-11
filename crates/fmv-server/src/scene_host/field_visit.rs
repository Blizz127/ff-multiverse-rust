//! Evaluate an idle field operation against its complete stored predecessor, then freeze
//! one atomic visit save. Public interpreter result structs are deliberately not inputs:
//! this module reads the bound source and executes the checked path itself.
//!
//! A source binding proves which bytes/init profile were selected, not that an opaque
//! entry or anchor can be restored. Those bytes are preserved unchanged. Runtime visit
//! admission stays closed until a reviewed reconstruction profile and playback exist.

use fmv_core::GameId;
use fmv_ff7::field::FieldFile;
use fmv_ff7::script::{FieldScript, FieldScratch, FieldScratchError, FieldScratchOutcome};
use fmv_ff7::script::dialogue::{run_talk_exact_with_scratch, End, ExactError};
use fmv_ff7::script::scene::{run_to_end_with_scratch_strict, SceneDef, SceneError,
    SceneResources, ScriptRef, SKIPPED_STATE_OPS, REPLAY_VERSION};
use fmv_ff7::story::{canon_party, compare_exchange_progression, party_after,
    party_visibility_matches, StoryState};
use fmv_persist::{FieldVisitOperation, FieldVisitOperationKind, SceneState,
    MAX_FIELD_VISIT_FIELD_BYTES};

use crate::saver::{FieldVisitSaveJob, SceneSaveError};

#[derive(Debug, thiserror::Error)]
pub(crate) enum IdleVisitError {
    #[error("the field source or initialization profile is invalid")]
    Source,
    #[error("the stored visit does not match this field source/profile")]
    SourceChanged,
    #[error("an idle checked operation requires a coherent extended story, character and visit")]
    State,
    #[error("the operation is not a complete supported idle result")]
    Result,
    #[error(transparent)]
    Scratch(#[from] FieldScratchError),
    #[error(transparent)]
    Scene(#[from] SceneError),
    #[error(transparent)]
    Exact(#[from] ExactError),
    #[error(transparent)]
    Save(#[from] SceneSaveError),
}

/// Actual field bytes and an explicit versioned init/reconstruction-profile descriptor.
/// The descriptor belongs to trusted server code, never a client-provided approval flag.
/// No production reconstruction profile is selected by this constructor. Callers must
/// separately establish accepted entry, idle control, trigger and anchor provenance.
pub(crate) struct IdleVisitSource<'a> {
    key: String,
    field: &'a FieldFile,
    init: Vec<ScriptRef>,
    identity: [u8; 32],
}

impl<'a> IdleVisitSource<'a> {
    pub(crate) fn new(field_key: &str, field: &'a FieldFile, init: &[ScriptRef], profile: &[u8])
        -> Result<Self, IdleVisitError> {
        if field_key.is_empty() || field_key.len() > MAX_FIELD_VISIT_FIELD_BYTES
            || field_key.chars().any(char::is_control) || field_key != field_key.to_ascii_lowercase()
            || profile.is_empty() || profile.len() > 1024 || init.len() > 256 {
            return Err(IdleVisitError::Source);
        }
        let script = FieldScript::from_field(field).map_err(|_| IdleVisitError::Source)?;
        if init.iter().any(|r| script.span(usize::from(r.entity), usize::from(r.slot)).is_none()) {
            return Err(IdleVisitError::Source);
        }
        let mut hash = blake3::Hasher::new();
        // Bump the domain when the checked execution/init-profile contract changes.
        hash.update(b"fmv-ff7/idle-visit-source\0v1\0checked-scratch-v1\0");
        hash.update(&REPLAY_VERSION.to_le_bytes());
        for bytes in [field_key.as_bytes(), field.bytes(), script.bytes(), profile] {
            hash.update(&(bytes.len() as u64).to_le_bytes());
            hash.update(bytes);
        }
        hash.update(&(init.len() as u64).to_le_bytes());
        for r in init { hash.update(&[r.entity, r.slot]); }
        Ok(Self { key: field_key.into(), field, init: init.to_vec(), identity: *hash.finalize().as_bytes() })
    }

    pub(crate) fn identity(&self) -> [u8; 32] { self.identity }

    fn predecessor(&self, expected: &SceneState) -> Result<(StoryState, FieldScratch, u32), IdleVisitError> {
        let pair = &expected.pair;
        let character = pair.character.as_ref().ok_or(IdleVisitError::State)?;
        let visit = expected.field_visit.as_ref().ok_or(IdleVisitError::State)?;
        if pair.story.game != GameId::Ff7 || character.game != GameId::Ff7
            || character.account_id != pair.story.account_id || expected.checkpoint.is_some()
            || expected.reservation.is_some() || visit.active_continuation.is_some() {
            return Err(IdleVisitError::State);
        }
        if visit.field != self.key || visit.source_identity != self.identity { return Err(IdleVisitError::SourceChanged); }
        let story = StoryState::decode(&pair.story.blob).filter(StoryState::supports_extended)
            .ok_or(IdleVisitError::State)?;
        if story.encode() != pair.story.blob || !super::persistable_field_gil(character.gil, character.gil) {
            return Err(IdleVisitError::State);
        }
        let scratch = FieldScratch::from_parts(visit.scratch_values, visit.scratch_known_masks)?;
        Ok((story, scratch, character.gil))
    }

    /// Evaluate a completed scene without a battle or transfer. The definition is trusted
    /// server policy; its actual field and setup must match this source profile. Accepted
    /// movement/trigger and runtime presentation are not reconstructed by this method.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn scene_job(&self, def: &SceneDef, expected: &SceneState, operation: [u8; 16],
        seed: u32, answers: &[usize], now: u64) -> Result<FieldVisitSaveJob, IdleVisitError> {
        let (story, scratch, gil) = self.predecessor(expected)?;
        if def.game != "ff7" || def.field != self.key || def.setup != self.init
            || !def.offered_to(&story) || def.prepare_story(&story).map_err(|_| IdleVisitError::State)? != story {
            return Err(IdleVisitError::Source);
        }
        let outcome = run_to_end_with_scratch_strict(self.field, def, &story, seed,
            SceneResources { gil: Some(gil) }, &scratch, answers)?;
        if outcome.end != End::Finished || outcome.jump.is_some() || !outcome.is_complete()
            || !outcome.unknown_persistent_reads.is_empty()
            || [&outcome.setup_stats, &outcome.stats].iter().any(|stats| stats.ignored.keys().any(|op| SKIPPED_STATE_OPS.contains(op)))
            || outcome.writes.untracked.keys().any(|(bank, _)| fmv_ff7::story::canonical_persistent_bank(*bank).is_some())
            || outcome.resources.gil_before != Some(gil) {
            return Err(IdleVisitError::Result);
        }
        let after_gil = outcome.resources.gil_after.ok_or(IdleVisitError::Result)?;
        let effect = outcome.scratch.as_ref().ok_or(IdleVisitError::Result)?.as_ref().map_err(|error| IdleVisitError::Scratch(*error))?;
        let mut after = story.clone();
        outcome.writes.apply_extended_to(&mut after, &story).ok_or(IdleVisitError::Result)?;
        if !outcome.party_is_the_storys(&story, &after) { return Err(IdleVisitError::Result); }
        self.finish(expected, operation, &scratch, effect, after, after_gil, now)
    }

    /// The exact ordinary talk path has no economic or battle input. Unsupported resource
    /// instructions refuse in that interpreter; unchanged stored resources are retained here.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn talk_job(&self, entity: usize, expected: &SceneState, operation: [u8; 16],
        seed: u32, answers: &[usize], now: u64) -> Result<FieldVisitSaveJob, IdleVisitError> {
        let (story, scratch, gil) = self.predecessor(expected)?;
        let script = FieldScript::from_field(self.field).map_err(|_| IdleVisitError::Source)?;
        let run = run_talk_exact_with_scratch(&script, entity, &story, &scratch, seed, answers)?;
        if run.battle.is_some() || run.battle_story.is_some() || run.battle_scratch.is_some() {
            return Err(IdleVisitError::Result);
        }
        let effect = run.scratch.as_ref().ok_or(IdleVisitError::Result)?;
        let mut after = story.clone();
        if story.supports_native_party() {
            if !run.party.is_empty() { return Err(IdleVisitError::Result); }
            run.native.as_ref().ok_or(IdleVisitError::Result)?.apply_to(&mut after, &story)
                .map_err(|_| IdleVisitError::Result)?;
        } else {
            if run.native.is_some() { return Err(IdleVisitError::Result); }
            for write in &run.writes {
                after.write_masked(write.bank, write.address, write.value, write.mask).map_err(|_| IdleVisitError::Result)?;
            }
            if party_after(&canon_party(&story), &run.party) != canon_party(&after)
                || !party_visibility_matches(&story, &after, &run.party) { return Err(IdleVisitError::Result); }
        }
        self.finish(expected, operation, &scratch, effect, after, gil, now)
    }

    #[allow(clippy::too_many_arguments)]
    fn finish(&self, expected: &SceneState, operation: [u8; 16], before_scratch: &FieldScratch,
        effect: &FieldScratchOutcome, after_story: StoryState, after_gil: u32, now: u64)
        -> Result<FieldVisitSaveJob, IdleVisitError> {
        let mut after_scratch = before_scratch.clone();
        effect.apply_to(&mut after_scratch, before_scratch)?;
        let mut next = expected.clone();
        let before_blob = &expected.pair.story.blob;
        let after_blob = after_story.encode();
        if compare_exchange_progression(before_blob, before_blob, &after_blob).as_deref() != Some(after_blob.as_slice()) {
            return Err(IdleVisitError::Result);
        }
        next.pair.story.blob = after_blob;
        let character = next.pair.character.as_mut().ok_or(IdleVisitError::State)?;
        if !super::persistable_field_gil(character.gil, after_gil) { return Err(IdleVisitError::Result); }
        character.gil = after_gil;
        let visit = next.field_visit.as_mut().ok_or(IdleVisitError::State)?;
        visit.revision = visit.revision.checked_add(1).ok_or(IdleVisitError::State)?;
        visit.scratch_values = *after_scratch.values();
        visit.scratch_known_masks = *after_scratch.known_masks();
        Ok(FieldVisitSaveJob::new(FieldVisitOperation { id: operation, kind: FieldVisitOperationKind::Advance },
            expected.clone(), next, now)?)
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod owned_tests;
