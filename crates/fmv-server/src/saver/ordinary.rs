//! Acknowledged ordinary story writes and story-battle outcomes. Repeated field transitions
//! use a single-record CAS without receipts; resource deltas require a durable batch receipt.

use fmv_adapter_api::GameAdapter;
use fmv_core::{AccountId, GameId, MAX_INVENTORY_STACKS};
use fmv_persist::{ProgressPair, SceneBatchCommitted, SceneCommit, Store};
use fmv_protocol::{BattleRules, MAX_STORY_PROGRESS_BYTES};
use serde::Serialize;

use super::{SceneBattleSettlement, SceneSaveError, SceneSaved};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoryCasJob {
    pub account: AccountId,
    pub expected_story: Vec<u8>,
    pub next_story: Vec<u8>,
}

impl StoryCasJob {
    pub(super) fn validate(&self) -> Result<(), SceneSaveError> {
        if self.expected_story.len() > MAX_STORY_PROGRESS_BYTES || self.next_story.len() > MAX_STORY_PROGRESS_BYTES {
            return Err(SceneSaveError::InvalidRequest);
        }
        Ok(())
    }
}

fn validate_transition(adapter: &dyn GameAdapter, game: GameId, expected: &[u8], next: &[u8]) -> Result<(), SceneSaveError> {
    if adapter.game() != game || adapter.compare_exchange_progression(expected, expected, next).as_deref() != Some(next) {
        return Err(SceneSaveError::InvalidRequest);
    }
    Ok(())
}

/// A pure value change is idempotent without a permanent receipt: storage checks the exact
/// predecessor or recognizes that the desired value is already present. It never merges bits.
pub(crate) fn persist_story_cas(store: &Store, game: GameId, adapter: &dyn GameAdapter, job: &StoryCasJob, now: u64)
    -> Result<SceneSaved, SceneSaveError> {
    job.validate()?;
    validate_transition(adapter, game, &job.expected_story, &job.next_story)?;
    let (commit, pair) = store.compare_exchange_progression(job.account, game, &job.expected_story, &job.next_story, now)?;
    Ok(SceneSaved { commit, pair })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct StoryBattleMember {
    /// Coherent terminal snapshot, after every accepted item-use save ahead of it in the FIFO.
    pub expected: ProgressPair,
    /// Ordinary battle item costs have already been saved; consumed_items MUST be empty here.
    pub battle: SceneBattleSettlement,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct StoryBattleJob {
    /// Fresh per battle, stable while a terminal write is pending or retried. Never an EntityId.
    pub instance: [u8; 32],
    pub owner: AccountId,
    pub expected_story: Vec<u8>,
    pub next_story: Vec<u8>,
    pub members: Vec<StoryBattleMember>,
}

impl StoryBattleJob {
    pub(super) fn validate(&self) -> Result<(), SceneSaveError> {
        super::continuation::validate_accounts(&self.accounts())?;
        if self.instance == [0; 32] || self.expected_story.len() > MAX_STORY_PROGRESS_BYTES
            || self.next_story.len() > MAX_STORY_PROGRESS_BYTES {
            return Err(SceneSaveError::InvalidRequest);
        }
        let owner = self.members.iter().find(|member| member.expected.story.account_id == self.owner)
            .ok_or(SceneSaveError::InvalidRequest)?;
        if owner.expected.story.blob != self.expected_story { return Err(SceneSaveError::InvalidRequest); }
        let game = owner.expected.story.game;
        for member in &self.members {
            let record = member.expected.character.as_ref().ok_or(SceneSaveError::MissingCharacter)?;
            if member.expected.story.game != game || member.expected.story.blob.len() > MAX_STORY_PROGRESS_BYTES
                || record.account_id != member.expected.story.account_id || record.game != game
                || record.inventory.as_ref().is_some_and(|items| items.len() > MAX_INVENTORY_STACKS)
                || member.battle.reward.account != record.account_id
                || !member.battle.consumed_items.is_empty() || member.battle.reward.items.len() > MAX_INVENTORY_STACKS {
                return Err(SceneSaveError::InvalidRequest);
            }
        }
        Ok(())
    }

    fn accounts(&self) -> Vec<AccountId> { self.members.iter().map(|member| member.expected.story.account_id).collect() }

    fn selected_result(&self) -> Result<[u8; 32], SceneSaveError> {
        self.validate()?;
        let mut selected = self.clone();
        selected.members.sort_by_key(|member| member.expected.story.account_id);
        for member in &mut selected.members {
            member.expected.story.updated_at = 0;
            member.expected.story.schema_version = 0;
            if let Some(record) = &mut member.expected.character { record.updated_at = 0; record.schema_version = 0; }
        }
        let bytes = serde_json::to_vec(&selected).map_err(|_| SceneSaveError::InvalidRequest)?;
        let mut digest = blake3::Hasher::new();
        digest.update(b"fmv-ordinary-story-battle-v1\0");
        digest.update(&bytes);
        Ok(*digest.finalize().as_bytes())
    }
}

/// Commit each terminal reward/gauge and the owner's exact story together, once. Item uses
/// are deliberately absent: they reached storage earlier through ordinary accepted-command jobs.
pub(crate) fn persist_story_battle(store: &Store, game: GameId, rules: Option<&dyn BattleRules>, adapter: &dyn GameAdapter,
    job: &StoryBattleJob, now: u64) -> Result<SceneBatchCommitted, SceneSaveError> {
    let selected = job.selected_result()?;
    if job.members.iter().any(|member| member.expected.story.game != game) { return Err(SceneSaveError::InvalidRequest); }
    let accounts = job.accounts();
    if store.battle_outcome_applied(job.instance, &selected, &accounts, game)? {
        return Ok(SceneBatchCommitted { commit: SceneCommit::AlreadyApplied, current: store.get_scene_states(&accounts, game)? });
    }
    validate_transition(adapter, game, &job.expected_story, &job.next_story)?;
    let expected = job.members.iter().map(|member| member.expected.clone()).collect::<Vec<_>>();
    let current = store.get_scene_states(&accounts, game)?;
    if current.iter().zip(&expected).any(|(state, expected)| state.pair != *expected
        || state.checkpoint.is_some() || state.reservation.is_some()) { return Err(SceneSaveError::StaleState); }
    let rules = rules.ok_or(SceneSaveError::MissingRules)?;
    let mut next = expected.clone();
    for (pair, member) in next.iter_mut().zip(&job.members) {
        super::continuation::settle(pair.character.as_mut().ok_or(SceneSaveError::MissingCharacter)?, rules, &member.battle, now)?;
        if pair.story.account_id == job.owner && pair.story.blob != job.next_story {
            pair.story.blob = job.next_story.clone();
            pair.story.updated_at = now;
        }
    }
    Ok(store.commit_battle_outcome(job.instance, &selected, &expected, &next)?)
}

#[cfg(test)]
mod tests;
