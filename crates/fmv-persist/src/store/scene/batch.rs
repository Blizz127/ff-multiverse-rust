//! Account-owned continuations and all-participant battle transactions.
//!
//! The replay bytes are a versioned adapter transcript, never an interpreter heap. The
//! enclosing character record owns this replay metadata, which contains no runtime entity
//! ids. A private frozen roster contains account ids and blocks trash rekeying until the
//! group is released. Source changes must be refused by the adapter before replay.

#[cfg(test)]
mod tests;
#[cfg(test)]
#[path = "large_visit_tests.rs"]
mod large_visit_tests;

use fmv_core::{AccountId, GameId};
use serde::{Deserialize, Serialize};

use super::{ProgressPair, SceneCommit};

/// One battle's participating accounts, including its owner.
pub const MAX_SCENE_BATCH_ACCOUNTS: usize = 16;
/// Compact JSON checkpoint, including the original input and replay. The replay is itself
/// encoded data stored as a byte array: each byte can require four JSON bytes, so its bound
/// alone needs more than 1 MiB before the original story, landing and metadata are included.
/// The character-file cap separately allows pretty-printed arrays and retained receipts.
pub const MAX_SCENE_CHECKPOINT_BYTES: usize = 2 * 1024 * 1024;
/// Adapter-validated replay events, before the containing checkpoint JSON encodes these
/// bytes again. Fully known FF7 banks need about 5 KiB per battle snapshot; three victories,
/// windows, the frozen roster and inventories must fit without discarding known story bits.
/// This is not storage for raw VM memory or game assets. Wire frame limits still apply.
pub const MAX_SCENE_REPLAY_BYTES: usize = 256 * 1024;
/// A reviewed arrival permits three metadata steps. The adapter binds each ordinal to
/// its exact replay stage; Begin reserves these receipts and final completion together.
pub const MAX_ARRIVAL_METADATA_STEPS: u32 = 3;

/// Where an account's authoritative continuation stopped. A landing is kept durably; a
/// network acknowledgment alone cannot erase the only place a reconnect can recover. The
/// adapter may retire it only after verifying that the unchanged story's normal entry is
/// exactly this destination and that no required linked arrival remains.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum ScenePhase {
    BeforeBattle { ordinal: u32, attempt: u32, formation: u16, battle_seed: u64 },
    Resume { segment: u32 },
    Landing { destination: Vec<u8> },
}

/// One account's immutable replay input and current continuation. The adapter checks the
/// source hash and decodes `replay`/`destination` before it permits gameplay to resume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SceneCheckpoint {
    pub instance: [u8; 32],
    pub scene_key: String,
    pub source_hash: [u8; 32],
    pub replay_version: u32,
    pub original_story: Vec<u8>,
    pub original_gil: u32,
    pub seed: u32,
    pub replay: Vec<u8>,
    pub phase: ScenePhase,
    /// The contiguous prefix of battle ordinals whose results were durably consumed.
    pub consumed_battles: u32,
}

/// The scene owns its roster through battles and resumption, until final landing releases
/// the guests. Slot identity is stable within this group; there are no nested account ids.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SceneReservation {
    pub instance: [u8; 32],
    pub ordinal: u32,
    pub attempt: u32,
    pub slot: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SceneOperationKind {
    PrepareBattle, ResolveBattle, Finish, ContinueLanding,
    /// Finish an unreserved Resume, or retire a terminal Landing without changing its
    /// progress pair. The caller validates the latter's source and destination semantics;
    /// persistence checks that it only clears the singleton owner's metadata.
    Complete,
    /// An ordinary battle's complete terminal outcome, without a resumable scene checkpoint.
    BattleOutcome,
    /// Attach one idle field visit to an unreserved, noncombat Resume-0 checkpoint.
    /// The progress pair and visit contents stay unchanged except for its revision/link.
    AttachVisit,
    /// Record a newly accepted noncombat window prefix while keeping all gameplay state
    /// unchanged. The ordinal is the adapter-validated accepted window count, starting at 1.
    AdvanceVisit,
    /// Commit that same-field noncombat scene's checked afterimage and clear its link.
    /// The visit's entry, source, identity and reconstruction anchor remain unchanged.
    CompleteVisit,
    /// Convert a pending landing to a fresh unreserved arrival, reserving enough receipt
    /// capacity for all metadata steps and completion before admitting the arrival.
    BeginArrival,
    /// Persist an accepted arrival metadata step before an entry visit exists. Only replay
    /// changes; the exact progress pair and original noncombat checkpoint stay unchanged.
    AdvanceArrival,
    /// Finish that unreserved arrival and install its first idle visit in the same write
    /// as its story/gil result. A separate Complete followed by Enter cannot substitute.
    CompleteArrivalVisit,
}

/// Stable identity of one durable step. Retries reuse it; an authored repeated battle
/// increments `attempt` instead of reusing the receipt of an earlier attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SceneOperation {
    pub instance: [u8; 32],
    pub ordinal: u32,
    pub attempt: u32,
    pub kind: SceneOperationKind,
}

/// A coherent view of both records and the metadata a compare-and-swap must include.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SceneState {
    pub pair: ProgressPair,
    pub checkpoint: Option<SceneCheckpoint>,
    pub reservation: Option<SceneReservation>,
    /// Explicit durable field-entry state. Absence means no entry was recorded, never
    /// evidence of a zero initializer. Only explicit visit lifecycle operations change it;
    /// the ordinary scene operations preserve it unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field_visit: Option<crate::FieldVisitV1>,
}

/// Current state in the request's account order. A duplicate returns current progress,
/// never a stale after-image from before a later reward or continuation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SceneBatchCommitted {
    pub commit: SceneCommit,
    pub current: Vec<SceneState>,
}

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::atomic::Ordering;

use super::{CharacterRecord, Images, Store, digest, encoded, invalid, is_digest, INTENT_FILE, MAX_SCENE_RECEIPTS};
use crate::{PersistError, Result};
use crate::fsio;
use crate::model::{MAX_BLOB_BYTES, MAX_CHARACTER_RECORD_BYTES, MAX_RECORD_BYTES};

pub(super) const BATCH_INTENT_VERSION: u32 = 2;
// Sixteen pairs of full before/after images, bounded before the first durable write.
pub(super) const MAX_BATCH_INTENT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_BATTLES: u32 = 1024;
const MAX_ATTEMPTS: u32 = 1024;
const MAX_INPUT_BYTES: usize = 8 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RosterMember {
    account: AccountId,
    slot: u16,
}

/// Cohort size and stable slots keep a partial duplicate from acknowledging just its owner.
/// No account id appears here: completed receipts survive an explicit trash rekey.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BatchReceipt {
    result: String,
    members: u16,
    slot: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchRow {
    account: AccountId,
    game: GameId,
    before: Images,
    after: Images,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchIntent {
    operation: SceneOperation,
    receipt: String,
    result: String,
    rows: Vec<BatchRow>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchIntentFile {
    schema_version: u32,
    kind: String,
    checksum: String,
    intent: BatchIntent,
}

fn conflict(reason: &str) -> PersistError { PersistError::TransactionConflict(reason.into()) }

fn validate_operation(operation: &SceneOperation, result: &[u8]) -> Result<()> {
    if operation.instance == [0; 32] || operation.ordinal > MAX_BATTLES || operation.attempt > MAX_ATTEMPTS {
        return Err(invalid("invalid scene instance or operation boundary"));
    }
    if matches!(operation.kind, SceneOperationKind::BattleOutcome | SceneOperationKind::AttachVisit | SceneOperationKind::CompleteVisit | SceneOperationKind::CompleteArrivalVisit)
        && (operation.ordinal != 0 || operation.attempt != 0) {
        return Err(invalid("ordinary outcomes and idle visit operations require boundary zero"));
    }
    if matches!(operation.kind, SceneOperationKind::AdvanceVisit | SceneOperationKind::AdvanceArrival) && (operation.ordinal == 0 || operation.attempt != 0) {
        return Err(invalid("advancing a visit requires a positive accepted window prefix and attempt zero"));
    }
    if operation.kind == SceneOperationKind::AdvanceArrival && operation.ordinal > MAX_ARRIVAL_METADATA_STEPS {
        return Err(invalid("arrival metadata step exceeds its reviewed receipt budget"));
    }
    if operation.kind == SceneOperationKind::BeginArrival && operation.attempt != 0 {
        return Err(invalid("arrival admission requires attempt zero"));
    }
    if result.len() > MAX_BLOB_BYTES { return Err(invalid("batch selected result exceeds the blob limit")); }
    Ok(())
}

fn operation_key(operation: &SceneOperation) -> Result<String> {
    validate_operation(operation, &[])?;
    digest(&("fmv-scene-batch-operation-v1", operation))
}

fn selected_key(selected: &[u8]) -> Result<String> { digest(&("fmv-scene-batch-result-v1", selected)) }

fn validate_checkpoint(checkpoint: &SceneCheckpoint) -> Result<()> {
    super::validate_key(&checkpoint.scene_key, &[])?;
    if checkpoint.instance == [0; 32] || checkpoint.replay_version == 0
        || checkpoint.original_story.len() > MAX_INPUT_BYTES || checkpoint.replay.len() > MAX_SCENE_REPLAY_BYTES
        || checkpoint.consumed_battles > MAX_BATTLES {
        return Err(invalid("invalid or oversized scene checkpoint"));
    }
    match &checkpoint.phase {
        ScenePhase::BeforeBattle { ordinal, attempt, .. } => {
            if *ordinal != checkpoint.consumed_battles || *ordinal >= MAX_BATTLES || *attempt > MAX_ATTEMPTS {
                return Err(invalid("battle boundary does not follow its consumed prefix"));
            }
        }
        ScenePhase::Resume { segment } => {
            if *segment != checkpoint.consumed_battles { return Err(invalid("resume segment does not match its consumed prefix")); }
        }
        ScenePhase::Landing { destination } => {
            if destination.is_empty() || destination.len() > MAX_INPUT_BYTES { return Err(invalid("invalid or oversized pending landing")); }
        }
    }
    if serde_json::to_vec(checkpoint).map_err(|_| invalid("cannot encode checkpoint"))?.len() > MAX_SCENE_CHECKPOINT_BYTES {
        return Err(invalid("encoded checkpoint exceeds its limit"));
    }
    Ok(())
}

fn validate_reservation(reservation: &SceneReservation) -> Result<()> {
    if reservation.instance == [0; 32] || reservation.ordinal >= MAX_BATTLES || reservation.attempt > MAX_ATTEMPTS
        || usize::from(reservation.slot) >= MAX_SCENE_BATCH_ACCOUNTS {
        return Err(invalid("invalid scene reservation"));
    }
    Ok(())
}

fn boundary(checkpoint: &SceneCheckpoint) -> Option<(u32, u32)> {
    match checkpoint.phase { ScenePhase::BeforeBattle { ordinal, attempt, .. } => Some((ordinal, attempt)), _ => None }
}

pub(super) fn validate_record(record: &CharacterRecord) -> Result<()> {
    if record.scene_receipts.len() + record.batch_receipts.len() > MAX_SCENE_RECEIPTS
        || record.batch_receipts.iter().any(|(key, value)| !is_digest(key) || !is_digest(&value.result)
            || value.members == 0 || usize::from(value.members) > MAX_SCENE_BATCH_ACCOUNTS || usize::from(value.slot) >= MAX_SCENE_BATCH_ACCOUNTS) {
        return Err(invalid("invalid or oversized batch receipt set"));
    }
    if let Some(checkpoint) = &record.checkpoint {
        validate_checkpoint(checkpoint)?;
        if let Some((ordinal, attempt)) = boundary(checkpoint) {
            let reservation = record.reservation.as_ref().ok_or_else(|| invalid("battle owner has no reservation"))?;
            if (reservation.instance, reservation.ordinal, reservation.attempt) != (checkpoint.instance, ordinal, attempt) {
                return Err(invalid("owner reservation does not match its battle"));
            }
        } else if let Some(reservation) = &record.reservation {
            if !matches!(checkpoint.phase, ScenePhase::Resume { .. }) || checkpoint.instance != reservation.instance
                || reservation.ordinal.checked_add(1) != Some(checkpoint.consumed_battles) {
                return Err(invalid("checkpoint reservation does not match its completed battle"));
            }
        } else if matches!(checkpoint.phase, ScenePhase::Resume { .. }) && checkpoint.consumed_battles != 0 {
            return Err(invalid("resumed battle scene lost its frozen roster reservation"));
        }
    }
    if let Some(reservation) = &record.reservation {
        validate_reservation(reservation)?;
        let roster = record.scene_roster.as_ref().ok_or_else(|| invalid("reserved participant has no complete roster"))?;
        validate_roster(roster)?;
        if !roster.iter().any(|member| member.account == record.progress.account_id && member.slot == reservation.slot) {
            return Err(invalid("participant is missing from its battle roster"));
        }
    } else if record.scene_roster.is_some() { return Err(invalid("roster has no battle reservation")); }
    Ok(())
}

fn validate_roster(roster: &[RosterMember]) -> Result<()> {
    if roster.is_empty() || roster.len() > MAX_SCENE_BATCH_ACCOUNTS { return Err(invalid("invalid battle roster size")); }
    let mut accounts = BTreeSet::new();
    let mut slots = BTreeSet::new();
    let mut last = None;
    for member in roster {
        if !accounts.insert(member.account) || !slots.insert(member.slot) || usize::from(member.slot) >= MAX_SCENE_BATCH_ACCOUNTS
            || last.is_some_and(|prior| prior >= member.account) {
            return Err(invalid("battle roster is not canonical or has duplicate members or slots"));
        }
        last = Some(member.account);
    }
    Ok(())
}

impl Images {
    pub(in crate::store) fn state(&self, account: AccountId, game: GameId) -> SceneState {
        SceneState { pair: self.pair(account, game), checkpoint: self.character.as_ref().and_then(|r| r.checkpoint.clone()),
            reservation: self.character.as_ref().and_then(|r| r.reservation.clone()),
            field_visit: self.character.as_ref().and_then(|r| r.field_visit.clone()) }
    }
}

fn validate_accounts(accounts: &[AccountId]) -> Result<()> {
    if accounts.is_empty() || accounts.len() > MAX_SCENE_BATCH_ACCOUNTS
        || accounts.iter().copied().collect::<BTreeSet<_>>().len() != accounts.len() {
        return Err(invalid("batch must contain bounded, distinct accounts"));
    }
    Ok(())
}

fn identity(state: &SceneState) -> (AccountId, GameId) { (state.pair.story.account_id, state.pair.story.game) }

fn validate_states(states: &[SceneState]) -> Result<()> {
    let accounts: Vec<_> = states.iter().map(|s| identity(s).0).collect();
    validate_accounts(&accounts)?;
    let game = states.first().ok_or_else(|| invalid("empty batch"))?.pair.story.game;
    for state in states {
        let (account, member_game) = identity(state);
        if member_game != game { return Err(invalid("batch spans different games")); }
        state.pair.validate(account, game)?;
        crate::store::field_visit::validate_link(state.field_visit.as_ref(), state.checkpoint.as_ref(), state.reservation.as_ref())?;
        if state.pair.character.is_none() { return Err(invalid("batch requires stored character progress")); }
        if let Some(checkpoint) = &state.checkpoint { validate_checkpoint(checkpoint)?; }
        if let Some(reservation) = &state.reservation { validate_reservation(reservation)?; }
    }
    Ok(())
}

fn same_original(a: &SceneCheckpoint, b: &SceneCheckpoint) -> bool {
    a.instance == b.instance && a.scene_key == b.scene_key && a.source_hash == b.source_hash && a.replay_version == b.replay_version
        && a.original_story == b.original_story && a.original_gil == b.original_gil && a.seed == b.seed
}

fn roster_of(states: &[SceneState], instance: [u8; 32], ordinal: u32, attempt: u32) -> Result<Vec<RosterMember>> {
    let mut roster = Vec::with_capacity(states.len());
    for state in states {
        let r = state.reservation.as_ref().ok_or_else(|| invalid("battle participant has no reservation"))?;
        if (r.instance, r.ordinal, r.attempt) != (instance, ordinal, attempt) { return Err(invalid("mixed battle reservations")); }
        roster.push(RosterMember { account: identity(state).0, slot: r.slot });
    }
    roster.sort_by_key(|member| member.account);
    validate_roster(&roster)?;
    Ok(roster)
}

/// A narrow same-field noncombat lifecycle. The adapter binds the checkpoint source/replay
/// to the visit's field and reconstruction profile; those are distinct opaque identities.
/// No generic checkpoint operation may use this exemption to edit a visit.
fn validate_visit_transition(operation: &SceneOperation, before: &[SceneState], after: &[SceneState]) -> Result<Option<usize>> {
    let ([old], [new]) = (before, after) else { return Err(invalid("visit scene operations require a singleton owner")); };
    if operation.attempt != 0 || old.reservation.is_some() || new.reservation.is_some()
        || (if operation.kind == SceneOperationKind::AdvanceVisit { operation.ordinal == 0 } else { operation.ordinal != 0 }) {
        return Err(invalid("visit scene operations require an unreserved noncombat owner"));
    }
    let prior = old.field_visit.as_ref().ok_or_else(|| invalid("visit scene has no existing visit"))?;
    let next = new.field_visit.as_ref().ok_or_else(|| invalid("visit scene cannot discard its visit"))?;
    if prior.revision.checked_add(1) != Some(next.revision)
        || prior.visit_id != next.visit_id || prior.field != next.field || prior.source_identity != next.source_identity
        || prior.entry != next.entry || prior.anchor != next.anchor {
        return Err(invalid("visit scene must retain its identity, source, entry and anchor and advance exactly one revision"));
    }
    if prior.scratch_known_masks.iter().zip(&next.scratch_known_masks).any(|(old, new)| old & !new != 0) {
        return Err(invalid("visit scene cannot forget known scratch bits"));
    }
    let checkpoint = match operation.kind {
        SceneOperationKind::AttachVisit => {
            if old.checkpoint.is_some() || old.pair != new.pair
                || prior.scratch_values != next.scratch_values || prior.scratch_known_masks != next.scratch_known_masks {
                return Err(invalid("attaching a visit must preserve its idle progress pair and scratch exactly"));
            }
            new.checkpoint.as_ref().ok_or_else(|| invalid("attached visit has no checkpoint"))?
        }
        SceneOperationKind::AdvanceVisit => {
            let previous = old.checkpoint.as_ref().ok_or_else(|| invalid("visit advance has no attached checkpoint"))?;
            let checkpoint = new.checkpoint.as_ref().ok_or_else(|| invalid("visit advance cannot clear its checkpoint"))?;
            if old.pair != new.pair || prior.scratch_values != next.scratch_values || prior.scratch_known_masks != next.scratch_known_masks
                || !same_original(previous, checkpoint) || previous.phase != checkpoint.phase
                || previous.consumed_battles != checkpoint.consumed_battles || previous.replay == checkpoint.replay {
                return Err(invalid("visit advance may change only its replay payload and visit revision"));
            }
            previous
        }
        SceneOperationKind::CompleteVisit => {
            if new.checkpoint.is_some() { return Err(invalid("completing a visit must clear its checkpoint")); }
            old.checkpoint.as_ref().ok_or_else(|| invalid("visit completion has no attached checkpoint"))?
        }
        _ => return Err(invalid("operation cannot change a field visit")),
    };
    if checkpoint.instance != operation.instance || checkpoint.source_hash == [0; 32]
        || checkpoint.phase != (ScenePhase::Resume { segment: 0 }) || checkpoint.consumed_battles != 0
        || checkpoint.original_story != old.pair.story.blob
        || old.pair.character.as_ref().is_none_or(|character| checkpoint.original_gil != character.gil) {
        return Err(invalid("visit scene checkpoint must identify this noncombat instance and its exact original progress"));
    }
    // validate_states already checked both exact checkpoint/visit links, including None.
    Ok(Some(0))
}

/// An arrival has no accepted field visit yet. Its windows change only replay metadata;
/// completion publishes the checked story/gil and first idle visit as one atomic image.
/// The adapter validates the opaque arrival/anchor/source meanings and stable fresh ids.
fn validate_arrival_transition(operation: &SceneOperation, before: &[SceneState], after: &[SceneState]) -> Result<Option<usize>> {
    let ([old], [new]) = (before, after) else { return Err(invalid("arrival operations require a singleton owner")); };
    if old.field_visit.is_some() || old.reservation.is_some() || new.reservation.is_some() || operation.attempt != 0 {
        return Err(invalid("arrival operations require an unreserved owner with no accepted visit"));
    }
    let checkpoint = old.checkpoint.as_ref().ok_or_else(|| invalid("arrival has no checkpoint"))?;
    if operation.kind == SceneOperationKind::BeginArrival {
        let next = new.checkpoint.as_ref().ok_or_else(|| invalid("arrival admission requires its new checkpoint"))?;
        if new.field_visit.is_some() || old.pair != new.pair || checkpoint.instance != operation.instance
            || !matches!(checkpoint.phase, ScenePhase::Landing { .. }) || operation.ordinal != checkpoint.consumed_battles
            || next.instance == checkpoint.instance || next.source_hash == [0; 32]
            || next.phase != (ScenePhase::Resume { segment: 0 }) || next.consumed_battles != 0
            || next.original_story != old.pair.story.blob
            || old.pair.character.as_ref().is_none_or(|character| next.original_gil != character.gil) {
            return Err(invalid("arrival admission must replace one pending landing with a fresh checkpoint on its exact progress"));
        }
        return Ok(Some(0));
    }
    if checkpoint.instance != operation.instance || checkpoint.source_hash == [0; 32]
        || checkpoint.phase != (ScenePhase::Resume { segment: 0 }) || checkpoint.consumed_battles != 0
        || checkpoint.original_story != old.pair.story.blob
        || old.pair.character.as_ref().is_none_or(|character| checkpoint.original_gil != character.gil) {
        return Err(invalid("arrival checkpoint must identify this noncombat instance and exact original progress"));
    }
    match operation.kind {
        SceneOperationKind::AdvanceArrival => {
            let next = new.checkpoint.as_ref().ok_or_else(|| invalid("arrival advance cannot clear its checkpoint"))?;
            if operation.ordinal == 0 || old.pair != new.pair || new.field_visit.is_some()
                || !same_original(checkpoint, next) || checkpoint.phase != next.phase
                || checkpoint.consumed_battles != next.consumed_battles || checkpoint.replay == next.replay {
                return Err(invalid("arrival advance may change only its replay payload"));
            }
        }
        SceneOperationKind::CompleteArrivalVisit => {
            let visit = new.field_visit.as_ref().ok_or_else(|| invalid("arrival completion requires its first visit"))?;
            if operation.ordinal != 0 || new.checkpoint.is_some() || visit.revision != 0 || visit.active_continuation.is_some() {
                return Err(invalid("arrival completion must clear its checkpoint and install an idle revision-zero visit"));
            }
            let previous = old.pair.character.as_ref().ok_or_else(|| invalid("arrival has no character progress"))?;
            let next = new.pair.character.as_ref().ok_or_else(|| invalid("arrival has no character after-image"))?;
            let mut allowed = previous.clone();
            allowed.gil = next.gil;
            allowed.updated_at = next.updated_at;
            allowed.schema_version = next.schema_version;
            if allowed != *next { return Err(invalid("arrival completion may change only story, gil and write timestamps")); }
        }
        _ => return Err(invalid("operation cannot complete or advance an arrival")),
    }
    Ok(Some(0))
}

/// Check visible transitions independently of disk. The complete old roster is checked
/// separately against the private committed roster before an after-image is built.
fn validate_transition(operation: &SceneOperation, before: &[SceneState], after: &[SceneState]) -> Result<Option<usize>> {
    validate_states(before)?;
    validate_states(after)?;
    if before.len() != after.len() || before.iter().zip(after).any(|(a, b)| identity(a) != identity(b)) {
        return Err(invalid("batch after-images must have the same accounts and order"));
    }
    if matches!(operation.kind, SceneOperationKind::AttachVisit | SceneOperationKind::AdvanceVisit | SceneOperationKind::CompleteVisit) {
        return validate_visit_transition(operation, before, after);
    }
    if matches!(operation.kind, SceneOperationKind::BeginArrival | SceneOperationKind::AdvanceArrival | SceneOperationKind::CompleteArrivalVisit) {
        return validate_arrival_transition(operation, before, after);
    }
    if before.iter().zip(after).any(|(a, b)| a.field_visit != b.field_visit) {
        return Err(invalid("existing scene operations must preserve field visits unchanged"));
    }
    if operation.kind == SceneOperationKind::BattleOutcome {
        if operation.ordinal != 0 || operation.attempt != 0 {
            return Err(invalid("ordinary battle outcomes have one terminal operation per instance"));
        }
        if before.iter().chain(after).any(|state| state.checkpoint.is_some() || state.reservation.is_some()) {
            return Err(conflict("ordinary battle outcome cannot replace an active scene continuation"));
        }
        return Ok(None);
    }
    let owners: Vec<_> = before.iter().zip(after).enumerate().filter(|(_, (a, b))| a.checkpoint.is_some() || b.checkpoint.is_some()).map(|(index, _)| index).collect();
    let [owner] = owners.as_slice() else { return Err(invalid("batch requires exactly one checkpoint owner")); };
    let owner = *owner;
    let old = before.get(owner).ok_or_else(|| invalid("missing owner before-image"))?;
    let new = after.get(owner).ok_or_else(|| invalid("missing owner after-image"))?;
    if operation.kind == SceneOperationKind::Complete {
        let previous = old.checkpoint.as_ref().ok_or_else(|| invalid("completion has no checkpoint"))?;
        if before.len() != 1 || before.iter().chain(after).any(|state| state.reservation.is_some()) || new.checkpoint.is_some()
            || previous.instance != operation.instance
            || operation.ordinal != previous.consumed_battles || operation.attempt != 0 {
            return Err(invalid("completion must clear one unreserved owner at its current boundary"));
        }
        match &previous.phase {
            ScenePhase::Resume { .. } => {},
            ScenePhase::Landing { .. } if old.pair == new.pair => {},
            ScenePhase::Landing { .. } => return Err(invalid("landing retirement must preserve the entire progress pair")),
            ScenePhase::BeforeBattle { .. } => return Err(invalid("completion cannot discard an unresolved battle")),
        }
        return Ok(Some(owner));
    }
    let cp = new.checkpoint.as_ref().ok_or_else(|| invalid("missing owner checkpoint"))?;
    if operation.kind == SceneOperationKind::ContinueLanding {
        let previous = old.checkpoint.as_ref().ok_or_else(|| invalid("continuation has no pending landing"))?;
        if before.len() != 1 || before.iter().chain(after).any(|state| state.reservation.is_some())
            || previous.instance != operation.instance || !matches!(previous.phase, ScenePhase::Landing { .. })
            || operation.ordinal != previous.consumed_battles || operation.attempt != 0 || cp.instance == previous.instance
            || !matches!(cp.phase, ScenePhase::Resume { segment: 0 }) || cp.consumed_battles != 0
            || cp.original_story != old.pair.story.blob || old.pair.character.as_ref().is_none_or(|c| cp.original_gil != c.gil) {
            return Err(invalid("landing continuation must start a fresh scene instance on the exact landed progress"));
        }
        return Ok(Some(owner));
    }
    if cp.instance != operation.instance { return Err(invalid("operation belongs to another scene instance")); }
    for (index, state) in before.iter().enumerate() {
        if index != owner && state.checkpoint.is_some() { return Err(conflict("participant owns another scene continuation")); }
    }
    match &old.checkpoint {
        Some(previous) if !same_original(previous, cp) => return Err(conflict("scene replay original input changed")),
        None if cp.original_story != old.pair.story.blob || old.pair.character.as_ref().is_none_or(|c| cp.original_gil != c.gil)
            || cp.consumed_battles != 0 => return Err(invalid("new checkpoint does not match its original progress")),
        _ => {}
    }
    match operation.kind {
        SceneOperationKind::PrepareBattle => {
            if boundary(cp) != Some((operation.ordinal, operation.attempt)) { return Err(invalid("prepare operation has another battle boundary")); }
            roster_of(after, operation.instance, operation.ordinal, operation.attempt)?;
            match &old.checkpoint {
                None => {
                    if operation.ordinal != 0 || operation.attempt != 0 || before.iter().any(|s| s.reservation.is_some()) {
                        return Err(conflict("new scene cannot replace another reservation or skip a battle"));
                    }
                }
                Some(previous) => {
                    if cp.consumed_battles != previous.consumed_battles { return Err(invalid("prepare cannot consume a battle")); }
                    match previous.phase {
                        ScenePhase::Resume { .. } if operation.ordinal == previous.consumed_battles && operation.attempt == 0 => {
                            if let Some(reservation) = &old.reservation {
                                let old_roster = roster_of(before, operation.instance, reservation.ordinal, reservation.attempt)?;
                                if old_roster != roster_of(after, operation.instance, operation.ordinal, operation.attempt)? {
                                    return Err(conflict("next battle must retain its complete participants"));
                                }
                            } else if before.iter().any(|state| state.reservation.is_some()) {
                                return Err(conflict("new battle cannot take another scene's reserved participant"));
                            }
                        }
                        ScenePhase::BeforeBattle { ordinal, attempt, .. } if (ordinal, attempt) == (operation.ordinal, operation.attempt) => {
                            if roster_of(before, operation.instance, ordinal, attempt)? != roster_of(after, operation.instance, ordinal, attempt)? {
                                return Err(conflict("prepared retry cannot replace its participants"));
                            }
                        }
                        _ => return Err(conflict("checkpoint is not ready to prepare this battle")),
                    }
                }
            }
        }
        SceneOperationKind::ResolveBattle => {
            let previous = old.checkpoint.as_ref().ok_or_else(|| invalid("resolution has no prepared checkpoint"))?;
            if boundary(previous) != Some((operation.ordinal, operation.attempt)) { return Err(conflict("battle result is stale")); }
            let roster = roster_of(before, operation.instance, operation.ordinal, operation.attempt)?;
            match cp.phase {
                ScenePhase::Resume { .. } if cp.consumed_battles == previous.consumed_battles + 1 => {
                    if roster != roster_of(after, operation.instance, operation.ordinal, operation.attempt)? {
                        return Err(invalid("resolved scene must retain its complete frozen roster"));
                    }
                }
                ScenePhase::BeforeBattle { ordinal, attempt, .. } if cp.consumed_battles == previous.consumed_battles
                    && ordinal == operation.ordinal && operation.attempt.checked_add(1) == Some(attempt) => {
                    if roster != roster_of(after, operation.instance, ordinal, attempt)? { return Err(conflict("retry must keep its complete participants")); }
                }
                _ => return Err(invalid("resolution must consume one battle or prepare its next attempt")),
            }
        }
        SceneOperationKind::Finish => {
            if after.iter().any(|s| s.reservation.is_some())
                || !matches!(cp.phase, ScenePhase::Landing { .. }) || operation.ordinal != cp.consumed_battles || operation.attempt != 0 {
                return Err(invalid("finish must release the group and retain its owner's pending landing"));
            }
            if old.reservation.is_none() && before.len() != 1 {
                return Err(invalid("unreserved finish cannot change unrelated participants"));
            }
            if let Some(previous) = &old.checkpoint {
                if !matches!(previous.phase, ScenePhase::Resume { .. }) || cp.consumed_battles != previous.consumed_battles {
                    return Err(conflict("checkpoint is not ready to finish"));
                }
            }
        }
        SceneOperationKind::ContinueLanding | SceneOperationKind::Complete | SceneOperationKind::BattleOutcome
            | SceneOperationKind::AttachVisit | SceneOperationKind::AdvanceVisit | SceneOperationKind::CompleteVisit
                    | SceneOperationKind::BeginArrival | SceneOperationKind::AdvanceArrival | SceneOperationKind::CompleteArrivalVisit => return Err(invalid("invalid scene operation transition")),
    }
    Ok(Some(owner))
}

fn receipt_status(images: &[Images], receipt: &str, result: &str) -> Result<bool> {
    let mut found = 0usize;
    let mut slots = BTreeSet::new();
    for image in images {
        if let Some(recorded) = image.character.as_ref().and_then(|r| r.batch_receipts.get(receipt)) {
            if recorded.result != result || usize::from(recorded.members) != images.len() || !slots.insert(recorded.slot) {
                return Err(conflict("batch receipt records a different result or participant cohort"));
            }
            found += 1;
        }
    }
    if found == 0 { return Ok(false); }
    if found != images.len() { return Err(conflict("only part of the requested cohort has this batch receipt")); }
    Ok(true)
}

fn operation_minimum_index(operation: &SceneOperation) -> u32 {
    if matches!(operation.kind, SceneOperationKind::AttachVisit | SceneOperationKind::AdvanceVisit | SceneOperationKind::CompleteVisit
        | SceneOperationKind::BeginArrival | SceneOperationKind::AdvanceArrival | SceneOperationKind::CompleteArrivalVisit) {
        crate::index::FIELD_VISIT_INDEX_VERSION
    } else { crate::index::DURABLE_STORY_INDEX_VERSION }
}

/// Check the full remaining arrival budget against the exact private predecessor, not
/// the public SceneState (which deliberately omits receipt history). Retries bypass this
/// only after their existing receipt was verified. Recovery repeats the same preflight.
fn validate_receipt_capacity(record: &CharacterRecord, operation: &SceneOperation) -> Result<()> {
    let needed = match operation.kind {
        SceneOperationKind::BeginArrival => (MAX_ARRIVAL_METADATA_STEPS + 2) as usize,
        SceneOperationKind::AdvanceArrival => (MAX_ARRIVAL_METADATA_STEPS + 2).checked_sub(operation.ordinal)
            .ok_or_else(|| invalid("invalid arrival receipt budget"))? as usize,
        _ => 1,
    };
    let used = record.scene_receipts.len() + record.batch_receipts.len();
    if used > MAX_SCENE_RECEIPTS.saturating_sub(needed) {
        if operation.kind == SceneOperationKind::BattleOutcome {
            return Err(conflict("ordinary battle receipt capacity exhausted"));
        }
        return Err(invalid("scene receipt capacity exhausted for the remaining operation budget; receipts cannot be evicted"));
    }
    Ok(())
}

impl Store {
    /// Read a coherent roster which can accept one ordinary terminal battle outcome.
    /// Call before starting its engine or accepting item commands. The world must keep the
    /// accounts reserved until the acknowledged outcome; this is a capacity/precondition
    /// snapshot, not a durable reservation or permission to race another writer.
    pub fn get_battle_outcome_states(&self, accounts: &[AccountId], game: GameId) -> Result<Vec<SceneState>> {
        validate_accounts(accounts)?;
        let _read = self.progress_read()?;
        let images = self.read_batch(accounts, game)?;
        for image in &images {
            let character = image.character.as_ref().ok_or_else(|| conflict("ordinary battle requires stored character progress"))?;
            if character.has_continuation() || character.scene_roster.is_some() {
                return Err(conflict("ordinary battle cannot overlap an active scene continuation"));
            }
            if character.scene_receipts.len() + character.batch_receipts.len() >= MAX_SCENE_RECEIPTS {
                return Err(conflict("ordinary battle receipt capacity exhausted"));
            }
        }
        Ok(images.iter().zip(accounts).map(|(image, account)| image.state(*account, game)).collect())
    }

    /// Check an ordinary battle's complete terminal receipt before calculating its resource
    /// deltas again. `instance` is a fresh stable identity for that battle, reused on retries.
    pub fn battle_outcome_applied(&self, instance: [u8; 32], selected_result: &[u8], accounts: &[AccountId], game: GameId) -> Result<bool> {
        let operation = SceneOperation { instance, ordinal: 0, attempt: 0, kind: SceneOperationKind::BattleOutcome };
        self.scene_batch_applied(&operation, selected_result, accounts, game)
    }

    /// Commit one ordinary battle's exact story/resource after-images for its entire frozen
    /// roster. No scene checkpoint is created. The shared recoverable intent and per-member
    /// receipts make an acknowledged outcome durable and once-only across restart.
    /// Game rules must validate deltas and which participant owns any authored story change.
    pub fn commit_battle_outcome(&self, instance: [u8; 32], selected_result: &[u8], expected: &[ProgressPair], next: &[ProgressPair]) -> Result<SceneBatchCommitted> {
        let operation = SceneOperation { instance, ordinal: 0, attempt: 0, kind: SceneOperationKind::BattleOutcome };
        let accounts: Vec<_> = expected.iter().map(|pair| pair.story.account_id).collect();
        let game = expected.first().ok_or_else(|| invalid("empty battle outcome"))?.story.game;
        let current = self.get_scene_states(&accounts, game)?;
        if expected.len() != next.len() { return Err(invalid("battle outcome member counts differ")); }
        // Pair-only callers do not own visit metadata. Capture it coherently, then include
        // it in the exact CAS below: a concurrent visit change refuses, never overwrites.
        let states = |pairs: &[ProgressPair]| pairs.iter().zip(&current).map(|(pair, current)| SceneState {
            pair: pair.clone(), ..current.clone()
        }).collect::<Vec<_>>();
        self.commit_scene_batch(&operation, selected_result, &states(expected), &states(next))
    }

    /// Called with the writer and progress gates held. Ordinary setters cannot drift the
    /// story/resources from the immutable replay input of a held scene or battle.
    pub(in crate::store) fn ensure_no_scene_continuation(&self, account: AccountId, game: GameId) -> Result<()> {
        if CharacterRecord::read(&self.character_path(account.0, game)?)?.is_some_and(|r| r.has_continuation()) {
            return Err(conflict("account has an active scene continuation or battle reservation"));
        }
        Ok(())
    }

    pub fn get_scene_state(&self, account: AccountId, game: GameId) -> Result<SceneState> {
        let mut states = self.get_scene_states(&[account], game)?;
        states.pop().ok_or_else(|| invalid("missing requested scene state"))
    }

    /// Read the whole requested cohort under one shared progress gate.
    pub fn get_scene_states(&self, accounts: &[AccountId], game: GameId) -> Result<Vec<SceneState>> {
        validate_accounts(accounts)?;
        let _read = self.progress_read()?;
        self.read_batch(accounts, game).map(|images| images.iter().zip(accounts).map(|(image, account)| image.state(*account, game)).collect())
    }

    /// Recover the complete frozen battle from any member, without scanning accounts.
    /// The checkpoint owner is first, followed by the other members in roster-slot order.
    /// An account outside a battle returns its one current state (including a resumable
    /// scene or durable landing). Incomplete or conflicting group metadata fails closed.
    pub fn get_scene_group(&self, account: AccountId, game: GameId) -> Result<Vec<SceneState>> {
        let _read = self.progress_read()?;
        let initial = self.read_batch(&[account], game)?.pop().ok_or_else(|| invalid("missing requested account"))?;
        let Some(reservation) = initial.character.as_ref().and_then(|c| c.reservation.as_ref()) else {
            return Ok(vec![initial.state(account, game)]);
        };
        let roster = initial.character.as_ref().and_then(|c| c.scene_roster.as_ref()).ok_or_else(|| invalid("reservation has no recovery roster"))?;
        let mut members = roster.clone();
        members.sort_by_key(|member| member.slot);
        let accounts: Vec<_> = members.iter().map(|member| member.account).collect();
        let images = self.read_batch(&accounts, game)?;
        let mut states: Vec<_> = images.iter().zip(&accounts).map(|(image, account)| image.state(*account, game)).collect();
        let restored_roster = roster_of(&states, reservation.instance, reservation.ordinal, reservation.attempt)
            .map_err(|_| conflict("reserved battle group is incomplete or inconsistent"))?;
        if &restored_roster != roster || images.iter().any(|image| image.character.as_ref().and_then(|c| c.scene_roster.as_ref()) != Some(roster)) {
            return Err(conflict("reserved battle recovery rosters disagree"));
        }
        let owners: Vec<_> = states.iter().enumerate().filter_map(|(index, state)| state.checkpoint.as_ref().map(|checkpoint| (index, checkpoint))).collect();
        let [(owner, checkpoint)] = owners.as_slice() else { return Err(conflict("reserved battle must have exactly one checkpoint owner")); };
        let matching_phase = match checkpoint.phase {
            ScenePhase::BeforeBattle { ordinal, attempt, .. } => (ordinal, attempt) == (reservation.ordinal, reservation.attempt),
            ScenePhase::Resume { segment } => reservation.ordinal.checked_add(1) == Some(segment),
            ScenePhase::Landing { .. } => false,
        };
        if checkpoint.instance != reservation.instance || !matching_phase {
            return Err(conflict("reserved battle checkpoint has another boundary"));
        }
        let owner = *owner;
        if owner != 0 { let state = states.remove(owner); states.insert(0, state); }
        Ok(states)
    }

    fn read_batch(&self, accounts: &[AccountId], game: GameId) -> Result<Vec<Images>> {
        accounts.iter().map(|account| {
            if !self.index().contains_account(account.0) { return Err(PersistError::UnknownAccount(*account)); }
            self.read_images(*account, game)
        }).collect()
    }

    /// Check every participant's durable receipt before reconstructing rewards or item
    /// costs. A partial/different cohort is a conflict, never a successful owner-only retry.
    /// A matching receipt establishes the current compatibility fence before acknowledgment;
    /// a missing or conflicting receipt does not change the store format.
    pub fn scene_batch_applied(&self, operation: &SceneOperation, selected_result: &[u8], accounts: &[AccountId], game: GameId) -> Result<bool> {
        validate_accounts(accounts)?;
        validate_operation(operation, selected_result)?;
        let receipt = operation_key(operation)?;
        let result = selected_key(selected_result)?;
        let writer = self.writer()?;
        let _progress = self.progress_write();
        let current = self.read_batch(accounts, game)?;
        if operation.kind == SceneOperationKind::BattleOutcome
            && current.iter().any(|image| image.character.as_ref().is_some_and(CharacterRecord::has_continuation)) {
            return Err(conflict("ordinary battle outcome cannot overlap an active scene continuation"));
        }
        let applied = receipt_status(&current, &receipt, &result)?;
        if applied {
            let minimum = current.iter().map(Images::minimum_index_version).fold(operation_minimum_index(operation), u32::max);
            self.ensure_index_format(&writer, minimum)?;
        }
        Ok(applied)
    }

    pub fn commit_scene_batch(&self, operation: &SceneOperation, selected_result: &[u8], expected: &[SceneState], next: &[SceneState]) -> Result<SceneBatchCommitted> {
        self.commit_scene_batch_with_hook(operation, selected_result, expected, next, |_| Ok(()))
    }

    fn commit_scene_batch_with_hook(&self, operation: &SceneOperation, selected_result: &[u8], expected: &[SceneState], next: &[SceneState], mut hook: impl FnMut(u32) -> Result<()>) -> Result<SceneBatchCommitted> {
        validate_operation(operation, selected_result)?;
        let owner = validate_transition(operation, expected, next)?;
        let receipt = operation_key(operation)?;
        let result = selected_key(selected_result)?;
        let accounts: Vec<_> = expected.iter().map(|state| identity(state).0).collect();
        let game = expected.first().ok_or_else(|| invalid("empty batch"))?.pair.story.game;
        let _writer = self.writer()?;
        let _progress = self.progress_write();
        let before = self.read_batch(&accounts, game)?;
        if operation.kind == SceneOperationKind::BattleOutcome
            && before.iter().any(|image| image.character.as_ref().is_some_and(CharacterRecord::has_continuation)) {
            return Err(conflict("ordinary battle outcome cannot overlap an active scene continuation"));
        }
        if receipt_status(&before, &receipt, &result)? {
            let minimum = before.iter().map(Images::minimum_index_version).fold(operation_minimum_index(operation), u32::max);
            self.ensure_index_format(&_writer, minimum)?;
            return Ok(SceneBatchCommitted { commit: SceneCommit::AlreadyApplied,
                current: before.iter().zip(&accounts).map(|(image, account)| image.state(*account, game)).collect() });
        }
        if before.iter().zip(expected).any(|(image, state)| image.state(identity(state).0, game) != *state) {
            return Err(conflict("story, character or continuation precondition changed"));
        }
        let previous_owner = owner.and_then(|owner| before.get(owner)).and_then(|image| image.character.as_ref());
        if let Some(previous_owner) = previous_owner.filter(|previous_owner| previous_owner.scene_roster.is_some()) {
            let roster = previous_owner.scene_roster.as_ref().ok_or_else(|| invalid("missing checkpoint roster"))?;
            let reservation = previous_owner.reservation.as_ref().ok_or_else(|| invalid("roster has no reservation"))?;
            let (ordinal, attempt) = (reservation.ordinal, reservation.attempt);
            if *roster != roster_of(expected, operation.instance, ordinal, attempt)? {
                return Err(conflict("batch omits or substitutes a prepared participant"));
            }
        }
        let next_roster = owner.and_then(|owner| next.get(owner)).and_then(|s| s.reservation.as_ref())
            .map(|reservation| roster_of(next, reservation.instance, reservation.ordinal, reservation.attempt)).transpose()?;
        let mut rows = Vec::with_capacity(before.len());
        for (index, ((account, old), new)) in accounts.iter().zip(before).zip(next).enumerate() {
            let mut character = old.character.clone().ok_or_else(|| invalid("missing stored participant"))?;
            validate_receipt_capacity(&character, operation)?;
            let slot = match operation.kind {
                SceneOperationKind::PrepareBattle => new.reservation.as_ref().map(|r| r.slot),
                SceneOperationKind::ResolveBattle => old.character.as_ref().and_then(|r| r.reservation.as_ref()).map(|r| r.slot),
                SceneOperationKind::Finish => Some(old.character.as_ref().and_then(|r| r.reservation.as_ref()).map_or(0, |r| r.slot)),
                SceneOperationKind::ContinueLanding | SceneOperationKind::Complete
                    | SceneOperationKind::AttachVisit | SceneOperationKind::AdvanceVisit | SceneOperationKind::CompleteVisit
                    | SceneOperationKind::BeginArrival | SceneOperationKind::AdvanceArrival | SceneOperationKind::CompleteArrivalVisit => Some(0),
                SceneOperationKind::BattleOutcome => Some(index as u16),
            }.ok_or_else(|| invalid("missing participant slot"))?;
            character.progress = new.pair.character.clone().ok_or_else(|| invalid("missing character after-image"))?;
            character.checkpoint = new.checkpoint.clone();
            character.reservation = new.reservation.clone();
            character.field_visit = new.field_visit.clone();
            character.scene_roster = next_roster.clone();
            character.batch_receipts.insert(receipt.clone(), BatchReceipt { result: result.clone(), members: accounts.len() as u16, slot });
            character.stamp();
            let mut after = Images { story: Some(new.pair.story.clone()), character: Some(character) };
            if operation.kind == SceneOperationKind::CompleteArrivalVisit {
                if old.character.as_ref().is_some_and(|record| record.last_visit_commit.is_some()) {
                    return Err(invalid("arrival completion cannot replace a previous visit receipt"));
                }
                let entry_receipt = crate::store::field_visit::arrival_receipt(operation, &result,
                    &old.state(*account, game), &after.state(*account, game))?;
                after.character.as_mut().ok_or_else(|| invalid("missing arrival character"))?.last_visit_commit = Some(entry_receipt);
            }
            after.character.as_ref().ok_or_else(|| invalid("missing character after-image"))?.validate()?;
            rows.push(BatchRow { account: *account, game, before: old, after });
        }
        let intent = BatchIntent { operation: *operation, receipt, result, rows };
        self.validate_batch_intent(&intent)?;
        self.encode_batch_after(&intent)?;
        let file = BatchIntentFile { schema_version: BATCH_INTENT_VERSION, kind: "fmv-scene-batch-transaction".into(), checksum: digest(&intent)?, intent };
        let path = self.root.join(INTENT_FILE);
        let bytes = encoded(&path, &file, MAX_BATCH_INTENT_BYTES)?;
        hook(0)?;
        let minimum = file.intent.rows.iter().flat_map(|row| [&row.before, &row.after])
            .map(Images::minimum_index_version).fold(operation_minimum_index(operation), u32::max);
        self.ensure_index_format(&_writer, minimum)?;
        self.transaction_poisoned.store(true, Ordering::Release);
        fsio::atomic_write(&path, &bytes)?;
        hook(1)?;
        self.install_batch_after(&file.intent, &mut hook)?;
        std::fs::remove_file(&path).map_err(|e| PersistError::io("remove scene batch intent", &path, e))?;
        hook(2 + file.intent.rows.len() as u32 * 2)?;
        fsio::sync_dir(&self.root).map_err(|e| PersistError::io("sync scene batch intent removal", &self.root, e))?;
        hook(3 + file.intent.rows.len() as u32 * 2)?;
        self.transaction_poisoned.store(false, Ordering::Release);
        Ok(SceneBatchCommitted { commit: SceneCommit::Applied,
            current: file.intent.rows.iter().map(|row| row.after.state(row.account, row.game)).collect() })
    }

    fn validate_batch_intent(&self, intent: &BatchIntent) -> Result<()> {
        if operation_key(&intent.operation)? != intent.receipt || !is_digest(&intent.result) { return Err(invalid("invalid batch receipt or result")); }
        let before: Vec<_> = intent.rows.iter().map(|row| row.before.state(row.account, row.game)).collect();
        let after: Vec<_> = intent.rows.iter().map(|row| row.after.state(row.account, row.game)).collect();
        let owner = validate_transition(&intent.operation, &before, &after)?;
        let next_roster = owner.and_then(|owner| after.get(owner)).and_then(|s| s.reservation.as_ref())
            .map(|reservation| roster_of(&after, reservation.instance, reservation.ordinal, reservation.attempt)).transpose()?;
        for (index, row) in intent.rows.iter().enumerate() {
            row.before.pair(row.account, row.game).validate(row.account, row.game)?;
            row.after.pair(row.account, row.game).validate(row.account, row.game)?;
            if row.after.story.is_none() { return Err(invalid("batch has no story after-image")); }
            let old = row.before.character.as_ref().ok_or_else(|| invalid("batch has no character before-image"))?;
            let new = row.after.character.as_ref().ok_or_else(|| invalid("batch has no character after-image"))?;
            old.validate()?;
            new.validate()?;
            validate_receipt_capacity(old, &intent.operation)?;
            if intent.operation.kind == SceneOperationKind::CompleteArrivalVisit {
                let entry_receipt = crate::store::field_visit::arrival_receipt(&intent.operation, &intent.result,
                    &row.before.state(row.account, row.game), &row.after.state(row.account, row.game))?;
                if old.last_visit_commit.is_some() || new.last_visit_commit.as_ref() != Some(&entry_receipt) {
                    return Err(invalid("arrival must create exactly its checked entry receipt"));
                }
            } else if old.last_visit_commit != new.last_visit_commit {
                return Err(invalid("batch must preserve the last field visit receipt"));
            }
            if old.scene_receipts != new.scene_receipts { return Err(invalid("batch must preserve ordinary scene receipts")); }
            let mut receipts = old.batch_receipts.clone();
            let slot = match intent.operation.kind {
                SceneOperationKind::PrepareBattle => new.reservation.as_ref().map(|r| r.slot),
                SceneOperationKind::ResolveBattle => old.reservation.as_ref().map(|r| r.slot),
                SceneOperationKind::Finish => Some(old.reservation.as_ref().map_or(0, |r| r.slot)),
                SceneOperationKind::ContinueLanding | SceneOperationKind::Complete
                    | SceneOperationKind::AttachVisit | SceneOperationKind::AdvanceVisit | SceneOperationKind::CompleteVisit
                    | SceneOperationKind::BeginArrival | SceneOperationKind::AdvanceArrival | SceneOperationKind::CompleteArrivalVisit => Some(0),
                SceneOperationKind::BattleOutcome => Some(index as u16),
            }.ok_or_else(|| invalid("batch receipt has no slot"))?;
            let added = BatchReceipt { result: intent.result.clone(), members: intent.rows.len() as u16, slot };
            if receipts.insert(intent.receipt.clone(), added).is_some() || receipts != new.batch_receipts {
                return Err(invalid("batch must append exactly its one new operation receipt per participant"));
            }
            if new.scene_roster != next_roster { return Err(invalid("batch roster after-image does not match participants")); }
            if let Some(roster) = &old.scene_roster {
                let reservation = old.reservation.as_ref().ok_or_else(|| invalid("old roster has no battle reservation"))?;
                if *roster != roster_of(&before, intent.operation.instance, reservation.ordinal, reservation.attempt)? {
                    return Err(invalid("batch omits prepared participants"));
                }
            }
        }
        Ok(())
    }

    fn encode_batch_after(&self, intent: &BatchIntent) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        intent.rows.iter().map(|row| {
            let story = row.after.story.as_ref().ok_or_else(|| invalid("missing batch story"))?;
            let character = row.after.character.as_ref().ok_or_else(|| invalid("missing batch character"))?;
            Ok((encoded(&self.progression_path(row.account.0, row.game)?, story, MAX_RECORD_BYTES)?,
                encoded(&self.character_path(row.account.0, row.game)?, character, MAX_CHARACTER_RECORD_BYTES)?))
        }).collect()
    }

    fn install_batch_after(&self, intent: &BatchIntent, hook: &mut impl FnMut(u32) -> Result<()>) -> Result<()> {
        let bytes = self.encode_batch_after(intent)?;
        for (index, (row, (story, character))) in intent.rows.iter().zip(bytes).enumerate() {
            fsio::ensure_dir(&self.progression_dir(row.account.0))?;
            fsio::atomic_write(&self.progression_path(row.account.0, row.game)?, &story)?;
            hook(2 + index as u32 * 2)?;
            fsio::atomic_write(&self.character_path(row.account.0, row.game)?, &character)?;
            hook(3 + index as u32 * 2)?;
        }
        Ok(())
    }

    pub(super) fn recover_scene_batch(&self, path: &Path, bytes: &[u8]) -> Result<()> {
        if self.index().format_version() < crate::index::SCENE_BATCH_INDEX_VERSION {
            return Err(PersistError::corrupt(path, "scene batch intent lacks its required index format fence"));
        }
        let file: BatchIntentFile = serde_json::from_slice(bytes).map_err(|e| PersistError::corrupt(path, format!("invalid scene batch intent: {e}")))?;
        if file.schema_version != BATCH_INTENT_VERSION || file.kind != "fmv-scene-batch-transaction" || file.checksum != digest(&file.intent)? {
            return Err(PersistError::corrupt(path, "scene batch intent kind, version or checksum mismatch"));
        }
        let required = file.intent.rows.iter().flat_map(|row| [&row.before, &row.after])
            .map(Images::minimum_index_version).max().unwrap_or(crate::model::INDEX_LOG_VERSION)
            .max(operation_minimum_index(&file.intent.operation));
        if required >= crate::index::FIELD_VISIT_INDEX_VERSION && self.index().format_version() < required {
            return Err(PersistError::corrupt(path, "visit-bearing batch intent lacks its required index format fence"));
        }
        let retires_landing = file.intent.operation.kind == SceneOperationKind::Complete
            && file.intent.rows.iter().any(|row| row.before.character.as_ref().and_then(|record| record.checkpoint.as_ref())
                .is_some_and(|checkpoint| matches!(checkpoint.phase, ScenePhase::Landing { .. })));
        if (file.intent.operation.kind == SceneOperationKind::BattleOutcome || retires_landing)
            && self.index().format_version() < crate::index::DURABLE_STORY_INDEX_VERSION {
            return Err(PersistError::corrupt(path, "durable story intent lacks its required index format fence"));
        }
        self.validate_batch_intent(&file.intent).map_err(|e| PersistError::corrupt(path, e.to_string()))?;
        // Check the entire cohort before installing any row: an unrelated third image
        // must not cause a partially applied recovery over its innocent peers.
        for row in &file.intent.rows {
            if !self.index().contains_account(row.account.0) { return Err(PersistError::corrupt(path, "scene batch participant is not live")); }
            let current = self.read_images(row.account, row.game)?;
            if (current.story != row.before.story && current.story != row.after.story)
                || (current.character != row.before.character && current.character != row.after.character) {
                return Err(PersistError::corrupt(path, "scene batch intent conflicts with unrelated progress; nothing overwritten"));
            }
        }
        self.install_batch_after(&file.intent, &mut |_| Ok(()))?;
        std::fs::remove_file(path).map_err(|e| PersistError::io("remove recovered scene batch intent", path, e))?;
        fsio::sync_dir(&self.root).map_err(|e| PersistError::io("sync recovered scene batch removal", &self.root, e))?;
        Ok(())
    }
}
