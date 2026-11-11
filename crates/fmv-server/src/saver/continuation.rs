//! Ordered, acknowledged scene checkpoints and shared battle outcomes.
//!
//! The world freezes each participating account before it obtains a queued snapshot. A checkpoint
//! commits before combat starts. During combat the world records accepted item uses locally; this
//! module applies those costs, rewards/drops and final gauges together with the resumed checkpoint.
//! An interrupted, uncommitted attempt therefore retains its starting resources; a committed attempt
//! is found by its durable receipt before any delta is calculated again.

use std::collections::HashSet;

use fmv_core::{AccountId, GameId};
use fmv_persist::{FieldVisitV1, SceneBatchCommitted, SceneCheckpoint, SceneCommit, SceneOperation, SceneOperationKind,
    ScenePhase, SceneReservation, SceneState, Store, MAX_FIELD_VISIT_ANCHOR_BYTES, MAX_FIELD_VISIT_BYTES,
    MAX_FIELD_VISIT_ENTRY_BYTES, MAX_FIELD_VISIT_FIELD_BYTES, MAX_ARRIVAL_METADATA_STEPS, MAX_SCENE_BATCH_ACCOUNTS,
    MAX_SCENE_CHECKPOINT_BYTES, MAX_SCENE_REPLAY_BYTES};
use fmv_protocol::{BattleReward, BattleRules};
use serde::Serialize;

use super::{progress, Change, Inventory, Progress, SceneSaveError};

/// The accepted effects for one frozen player in a shared battle. Rewards already include the
/// world's deterministic allocation/debug adjustments; the saver never rolls or splits them again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SceneBattleSettlement {
    pub character: u8,
    /// One item id per accepted command, in acceptance order. Drops cannot pay these costs.
    pub consumed_items: Vec<u16>,
    pub reward: BattleReward,
    /// None omits a battle gauge replacement, including actors without limits and debug-full-limit
    /// runs. Ordinary save normalization still clamps invalid stored values when another effect saves.
    pub final_limit: Option<u16>,
}

/// One member's exact predecessor and chosen changes. Checkpoint/reservation fields name the
/// complete next metadata, while absent story/gil values preserve those current record fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SceneBatchMember {
    pub expected: SceneState,
    pub next_story: Option<Vec<u8>>,
    pub next_gil: Option<u32>,
    pub checkpoint: Option<SceneCheckpoint>,
    pub reservation: Option<SceneReservation>,
    pub battle: Option<SceneBattleSettlement>,
    /// Only explicit visit-link or arrival-completion operations replace this metadata. Absence preserves it and
    /// omits the field from the decision encoding, retaining every pre-visit receipt hash.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_visit: Option<FieldVisitV1>,
}

/// One durable operation for the whole frozen roster. Its instance and attempt identities come
/// from the persisted scene, never a process-local SceneRunId, BattleId, or connected entity id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SceneBatchJob {
    pub operation: SceneOperation,
    pub members: Vec<SceneBatchMember>,
}

/// A single battle cannot enqueue an unbounded transcript of inventory commands.
pub(crate) const MAX_SCENE_BATTLE_ITEM_USES: usize = 4096;

pub(super) fn validate_accounts(accounts: &[AccountId]) -> Result<(), SceneSaveError> {
    if accounts.is_empty() || accounts.len() > MAX_SCENE_BATCH_ACCOUNTS
        || accounts.iter().copied().collect::<HashSet<_>>().len() != accounts.len() {
        return Err(SceneSaveError::InvalidRequest);
    }
    Ok(())
}

impl SceneBatchJob {
    pub(crate) fn validate(&self) -> Result<(), SceneSaveError> {
        validate_accounts(&self.members.iter().map(|m| m.expected.pair.story.account_id).collect::<Vec<_>>())?;
        let visit_operation = matches!(self.operation.kind,
            SceneOperationKind::AttachVisit | SceneOperationKind::AdvanceVisit | SceneOperationKind::CompleteVisit);
        let arrival_operation = matches!(self.operation.kind,
            SceneOperationKind::BeginArrival | SceneOperationKind::AdvanceArrival | SceneOperationKind::CompleteArrivalVisit);
        let valid_visit_ordinal = match self.operation.kind {
            SceneOperationKind::AdvanceVisit => (1..=fmv_protocol::MAX_SCENE_REPLAY_EVENTS as u32).contains(&self.operation.ordinal),
            // Two reviewed train windows, then the adapter-validated terminal presentation seal.
            SceneOperationKind::AdvanceArrival => (1..=MAX_ARRIVAL_METADATA_STEPS).contains(&self.operation.ordinal),
            // Beginning names the predecessor landing's consumed battle boundary, checked below.
            SceneOperationKind::BeginArrival => true,
            _ => self.operation.ordinal == 0,
        };
        if (visit_operation || arrival_operation) && (self.members.len() != 1 || self.operation.instance == [0; 32]
            || !valid_visit_ordinal || self.operation.attempt != 0) {
            return Err(SceneSaveError::InvalidRequest);
        }
        let valid_checkpoint = |checkpoint: &SceneCheckpoint| {
            !checkpoint.scene_key.is_empty() && checkpoint.scene_key.len() <= 256
                && !checkpoint.scene_key.chars().any(char::is_control)
                && checkpoint.original_story.len() <= fmv_protocol::MAX_STORY_PROGRESS_BYTES
                && checkpoint.replay.len() <= MAX_SCENE_REPLAY_BYTES
                && (match &checkpoint.phase {
                    // Storage bounds this small placement payload to the same 8 KiB as
                    // an original scene input, independently of the larger replay.
                    fmv_persist::ScenePhase::Landing { destination } => destination.len() <= fmv_protocol::MAX_STORY_PROGRESS_BYTES,
                    _ => true,
                })
                && serde_json::to_vec(checkpoint).is_ok_and(|bytes| bytes.len() <= MAX_SCENE_CHECKPOINT_BYTES)
        };
        for member in &self.members {
            if let Some(visit) = &member.expected.field_visit { validate_visit(visit)?; }
            if let Some(visit) = &member.next_visit { validate_visit(visit)?; }
            if arrival_operation { self.validate_arrival_transition(member)?; }
            else if visit_operation { self.validate_visit_transition(member)?; }
            else if member.next_visit.is_some() { return Err(SceneSaveError::InvalidRequest); }
            let owner = member.expected.checkpoint.is_some() || member.checkpoint.is_some();
            if member.expected.pair.story.blob.len() > fmv_protocol::MAX_STORY_PROGRESS_BYTES
                || member.expected.pair.character.as_ref().is_some_and(|record| {
                    record.account_id != member.expected.pair.story.account_id
                        || record.game != member.expected.pair.story.game
                        || record.inventory.as_ref().is_some_and(|items| items.len() > fmv_core::MAX_INVENTORY_STACKS)
                })
                || member.next_story.as_ref().is_some_and(|story| story.len() > fmv_protocol::MAX_STORY_PROGRESS_BYTES)
                || member.expected.checkpoint.as_ref().is_some_and(|c| !valid_checkpoint(c))
                || member.checkpoint.as_ref().is_some_and(|c| !valid_checkpoint(c)) {
                return Err(SceneSaveError::InvalidRequest);
            }
            if !owner && (member.next_story.as_ref().is_some_and(|story| *story != member.expected.pair.story.blob)
                || member.next_gil.is_some_and(|gil| member.expected.pair.character.as_ref().is_none_or(|record| record.gil != gil))) {
                // Authored scene changes belong to its owner; guests receive only their own battle settlement.
                return Err(SceneSaveError::InvalidRequest);
            }
            if let Some(battle) = &member.battle {
                if self.operation.kind != SceneOperationKind::ResolveBattle || member.next_gil.is_some()
                    || battle.reward.account != member.expected.pair.story.account_id
                    || battle.consumed_items.len() > MAX_SCENE_BATTLE_ITEM_USES
                    || battle.reward.items.len() > fmv_core::MAX_INVENTORY_STACKS {
                    return Err(SceneSaveError::InvalidRequest);
                }
            } else if self.operation.kind == SceneOperationKind::ResolveBattle {
                // Every member supplies an explicit outcome, even a zero reward and unchanged gauge.
                return Err(SceneSaveError::InvalidRequest);
            }
        }
        Ok(())
    }

    fn validate_visit_transition(&self, member: &SceneBatchMember) -> Result<(), SceneSaveError> {
        let old = member.expected.field_visit.as_ref().ok_or(SceneSaveError::InvalidRequest)?;
        let next = member.next_visit.as_ref().ok_or(SceneSaveError::InvalidRequest)?;
        let character = member.expected.pair.character.as_ref().ok_or(SceneSaveError::MissingCharacter)?;
        if member.expected.reservation.is_some() || member.reservation.is_some() || member.battle.is_some()
            || old.revision.checked_add(1) != Some(next.revision) || old.visit_id != next.visit_id
            || old.field != next.field || old.source_identity != next.source_identity || old.entry != next.entry
            || old.anchor != next.anchor
            || old.scratch_known_masks.iter().zip(&next.scratch_known_masks).any(|(old, next)| old & !next != 0) {
            return Err(SceneSaveError::InvalidRequest);
        }
        let checkpoint = match self.operation.kind {
            SceneOperationKind::AttachVisit => {
                if member.expected.checkpoint.is_some() || member.next_story.is_some() || member.next_gil.is_some()
                    || old.active_continuation.is_some() || next.active_continuation != Some(self.operation.instance)
                    || old.scratch_values != next.scratch_values || old.scratch_known_masks != next.scratch_known_masks {
                    return Err(SceneSaveError::InvalidRequest);
                }
                let checkpoint = member.checkpoint.as_ref().ok_or(SceneSaveError::InvalidRequest)?;
                if checkpoint.original_story != member.expected.pair.story.blob || checkpoint.original_gil != character.gil {
                    return Err(SceneSaveError::InvalidRequest);
                }
                checkpoint
            }
            SceneOperationKind::CompleteVisit => {
                if member.checkpoint.is_some() || old.active_continuation != Some(self.operation.instance)
                    || next.active_continuation.is_some() {
                    return Err(SceneSaveError::InvalidRequest);
                }
                member.expected.checkpoint.as_ref().ok_or(SceneSaveError::InvalidRequest)?
            }
            SceneOperationKind::AdvanceVisit => {
                if member.next_story.is_some() || member.next_gil.is_some()
                    || old.active_continuation != Some(self.operation.instance)
                    || next.active_continuation != old.active_continuation
                    || old.scratch_values != next.scratch_values || old.scratch_known_masks != next.scratch_known_masks {
                    return Err(SceneSaveError::InvalidRequest);
                }
                let before = member.expected.checkpoint.as_ref().ok_or(SceneSaveError::InvalidRequest)?;
                let after = member.checkpoint.as_ref().ok_or(SceneSaveError::InvalidRequest)?;
                let mut allowed = before.clone();
                allowed.replay.clone_from(&after.replay);
                if *after != allowed || before.replay == after.replay { return Err(SceneSaveError::InvalidRequest); }
                after
            }
            _ => return Err(SceneSaveError::InvalidRequest),
        };
        if checkpoint.instance != self.operation.instance || checkpoint.source_hash == [0; 32]
            || checkpoint.replay_version == 0 || checkpoint.phase != (ScenePhase::Resume { segment: 0 })
            || checkpoint.consumed_battles != 0 || checkpoint.original_story != member.expected.pair.story.blob
            || checkpoint.original_gil != character.gil {
            return Err(SceneSaveError::InvalidRequest);
        }
        Ok(())
    }

    /// Arrivals start from a durable landing continuation, before any field visit exists.
    /// Window acknowledgements publish only replay; completion creates the first idle visit.
    /// Source, transcript and scratch semantics belong to the checked adapter, not this queue.
    fn validate_arrival_transition(&self, member: &SceneBatchMember) -> Result<(), SceneSaveError> {
        let before = member.expected.checkpoint.as_ref().ok_or(SceneSaveError::InvalidRequest)?;
        let character = member.expected.pair.character.as_ref().ok_or(SceneSaveError::MissingCharacter)?;
        if member.expected.field_visit.is_some() || member.expected.reservation.is_some()
            || member.reservation.is_some() || member.battle.is_some() || before.instance != self.operation.instance {
            return Err(SceneSaveError::InvalidRequest);
        }
        if self.operation.kind == SceneOperationKind::BeginArrival {
            let after = member.checkpoint.as_ref().ok_or(SceneSaveError::InvalidRequest)?;
            if member.next_story.is_some() || member.next_gil.is_some() || member.next_visit.is_some()
                || !matches!(before.phase, ScenePhase::Landing { .. }) || self.operation.ordinal != before.consumed_battles
                || after.instance == [0; 32] || after.instance == before.instance || after.source_hash == [0; 32]
                || after.replay_version == 0 || after.phase != (ScenePhase::Resume { segment: 0 }) || after.consumed_battles != 0
                || after.original_story != member.expected.pair.story.blob || after.original_gil != character.gil {
                return Err(SceneSaveError::InvalidRequest);
            }
            return Ok(());
        }
        if before.source_hash == [0; 32]
            || before.replay_version == 0 || before.phase != (ScenePhase::Resume { segment: 0 })
            || before.consumed_battles != 0 || before.original_story != member.expected.pair.story.blob
            || before.original_gil != character.gil {
            return Err(SceneSaveError::InvalidRequest);
        }
        match self.operation.kind {
            SceneOperationKind::AdvanceArrival => {
                if member.next_story.is_some() || member.next_gil.is_some() || member.next_visit.is_some() {
                    return Err(SceneSaveError::InvalidRequest);
                }
                let after = member.checkpoint.as_ref().ok_or(SceneSaveError::InvalidRequest)?;
                let mut allowed = before.clone();
                allowed.replay.clone_from(&after.replay);
                if *after != allowed || before.replay == after.replay { return Err(SceneSaveError::InvalidRequest); }
            }
            SceneOperationKind::CompleteArrivalVisit => {
                let visit = member.next_visit.as_ref().ok_or(SceneSaveError::InvalidRequest)?;
                if member.checkpoint.is_some() || visit.revision != 0 || visit.active_continuation.is_some() {
                    return Err(SceneSaveError::InvalidRequest);
                }
            }
            _ => return Err(SceneSaveError::InvalidRequest),
        }
        Ok(())
    }

    fn accounts(&self) -> Vec<AccountId> {
        self.members.iter().map(|m| m.expected.pair.story.account_id).collect()
    }

    /// Receipt identity binds the complete decision, including costs and the next replay payload.
    /// Storage timestamps/schema stamps are not authored inputs. Sorting by account permits the
    /// caller to collect the same frozen roster in a different container order after reconnect.
    fn selected_result(&self) -> Result<[u8; 32], SceneSaveError> {
        self.validate()?;
        let mut normalized = self.clone();
        normalized.members.sort_by_key(|member| member.expected.pair.story.account_id.0);
        for member in &mut normalized.members {
            member.expected.pair.story.updated_at = 0;
            member.expected.pair.story.schema_version = 0;
            if let Some(character) = &mut member.expected.pair.character {
                character.updated_at = 0;
                character.schema_version = 0;
            }
        }
        let bytes = serde_json::to_vec(&normalized).map_err(|_| SceneSaveError::InvalidRequest)?;
        let mut digest = blake3::Hasher::new();
        digest.update(b"fmv-scene-batch-decision-v1\0");
        digest.update(&bytes);
        Ok(*digest.finalize().as_bytes())
    }
}

/// Bound opaque visit images before queueing. The adapter owns field/source/anchor semantics;
/// storage repeats structural validation and the exact link transition under its writer lock.
fn validate_visit(visit: &FieldVisitV1) -> Result<(), SceneSaveError> {
    if visit.schema_version != 1 || visit.visit_id == [0; 16] || visit.source_identity == [0; 32]
        || visit.entry.operation == [0; 16] || visit.active_continuation == Some([0; 32])
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

/// Apply one member's settlement with the same cap, experience-curve and inventory rules as normal
/// battle saves. A missing item is a stale/invalid attempt, never silently supplied by later drops.
pub(super) fn settle(
    record: &mut fmv_persist::CharacterProgress,
    rules: &dyn BattleRules,
    battle: &SceneBattleSettlement,
    now: u64,
) -> Result<(), SceneSaveError> {
    if battle.consumed_items.is_empty() && battle.reward.is_nothing() && battle.final_limit.is_none() {
        return Ok(());
    }
    let mut progress = Progress::from_record(record);
    progress.character = battle.character;
    progress::clamp(rules.progress_limits(), &mut progress);
    let mut inventory = Inventory::resolve(rules, record.inventory.as_deref());
    for &item in &battle.consumed_items {
        if !inventory.take_one(item) { return Err(SceneSaveError::InsufficientItems); }
    }
    if !battle.reward.is_nothing() {
        progress::apply_reward(rules, &mut progress, &battle.reward);
        inventory.apply(&Change::Found(battle.reward.items.clone()), rules.max_item_stack());
    }
    if let Some(limit) = battle.final_limit {
        progress.limit = limit.min(rules.progress_limits().max_limit);
    }
    let stored_inventory = (record.inventory.is_some() || !battle.consumed_items.is_empty() || !battle.reward.items.is_empty())
        .then(|| inventory.entries().to_vec());
    *record = progress.to_record(record.account_id, record.game, now);
    record.inventory = stored_inventory;
    Ok(())
}

/// Compute the same resource result the saver will commit, without changing storage. The world
/// uses this to put the owner's exact post-reward gil in the next replay event before submitting
/// its transaction. Its actual publication still waits for the durable acknowledgement.
pub(crate) fn preview_scene_settlement(
    record: &fmv_persist::CharacterProgress,
    rules: &dyn BattleRules,
    battle: &SceneBattleSettlement,
) -> Result<fmv_persist::CharacterProgress, SceneSaveError> {
    if battle.reward.account != record.account_id || battle.consumed_items.len() > MAX_SCENE_BATTLE_ITEM_USES
        || battle.reward.items.len() > fmv_core::MAX_INVENTORY_STACKS {
        return Err(SceneSaveError::InvalidRequest);
    }
    let mut next = record.clone();
    settle(&mut next, rules, battle, 0)?;
    Ok(next)
}

/// Blocking implementation used by the single ordered saver task. No error is retried here.
/// Metadata and all participants' after-images use the store's one recoverable batch intent.
pub(crate) fn persist_scene_batch(
    store: &Store,
    game: GameId,
    rules: Option<&dyn BattleRules>,
    job: &SceneBatchJob,
    now: u64,
) -> Result<SceneBatchCommitted, SceneSaveError> {
    if job.members.iter().any(|member| member.expected.pair.story.game != game) {
        return Err(SceneSaveError::InvalidRequest);
    }
    let selected = job.selected_result()?;
    let accounts = job.accounts();
    if store.scene_batch_applied(&job.operation, &selected, &accounts, game)? {
        return Ok(SceneBatchCommitted { commit: SceneCommit::AlreadyApplied, current: store.get_scene_states(&accounts, game)? });
    }
    let current = store.get_scene_states(&accounts, game)?;
    let expected = job.members.iter().map(|m| m.expected.clone()).collect::<Vec<_>>();
    if current != expected { return Err(SceneSaveError::StaleState); }
    let mut next = current;
    for (state, member) in next.iter_mut().zip(&job.members) {
        let character = state.pair.character.as_mut().ok_or(SceneSaveError::MissingCharacter)?;
        if let Some(battle) = &member.battle {
            settle(character, rules.ok_or(SceneSaveError::MissingRules)?, battle, now)?;
        }
        if let Some(gil) = member.next_gil {
            character.gil = gil;
            character.updated_at = now;
        }
        if let Some(story) = &member.next_story {
            state.pair.story.blob = story.clone();
            state.pair.story.updated_at = now;
        }
        state.checkpoint = member.checkpoint.clone();
        state.reservation = member.reservation.clone();
        if let Some(visit) = &member.next_visit { state.field_visit = Some(visit.clone()); }
    }
    Ok(store.commit_scene_batch(&job.operation, &selected, &expected, &next)?)
}

#[cfg(test)]
mod tests;
