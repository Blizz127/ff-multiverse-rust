//! Recoverable, once-only scene changes to the story and character records.
//!
//! A single durable intent contains both exact before/after images. It is written before
//! either record and removed only after both replacements and their directory syncs. On
//! reopen every current record must equal its before or after image; recovery then installs
//! both after images idempotently. An unrelated record, malformed intent or future schema
//! stops opening. An uncertain write poisons this Store until it is dropped and reopened.
//!
//! Ordinary receipts live in a private schema-2 envelope. Co-op checkpoints, reservations
//! and batch receipts require schema 3; explicit field visits and their single replaceable
//! receipt require schema 4, or schema 5 when either opaque visit payload exceeds 1024 bytes. Explicit durable story operations also establish a
//! recognized index format fence before their first intent or successful acknowledgment.
//! Ordinary character saves retain receipts; active continuations
//! refuse ordinary writes and account rekeying. Schema-1 intents remain recoverable alongside
//! the bounded schema-2 batch and schema-3 visit intents. Opening a store never opts it into
//! these new writes. Visit updates are idle-only until a separate runtime lifecycle is reviewed.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::Ordering;

use fmv_core::{AccountId, GameId};
use serde::{Deserialize, Serialize};

use super::Store;
use crate::{CharacterProgress, PersistError, Progression, Result};
use crate::fsio;
use crate::model::{MAX_BLOB_BYTES, MAX_CHARACTER_RECORD_BYTES, MAX_CHARACTER_PROGRESS_SCHEMA_VERSION, MAX_RECORD_BYTES, SCHEMA_VERSION};

mod batch;
#[cfg(test)]
mod character_write_tests;
pub use batch::{SceneBatchCommitted, SceneCheckpoint, SceneOperation, SceneOperationKind, ScenePhase, SceneReservation, SceneState,
    MAX_ARRIVAL_METADATA_STEPS, MAX_SCENE_BATCH_ACCOUNTS, MAX_SCENE_CHECKPOINT_BYTES, MAX_SCENE_REPLAY_BYTES};

pub(crate) const INTENT_FILE: &str = "pending-scene.json";
const INTENT_VERSION: u32 = 1;
const CHARACTER_RECEIPT_VERSION: u32 = 2;
const CHARACTER_CONTINUATION_VERSION: u32 = 3;
const CHARACTER_VISIT_VERSION: u32 = 4;
const MAX_INTENT_BYTES: u64 = 4 * 1024 * 1024;
/// Receipts are never evicted: exceeding this limit refuses the next transaction.
pub const MAX_SCENE_RECEIPTS: usize = 2048;

/// A coherent view of both progress records. An absent character remains absent, and the
/// story uses [`Progression::empty`] for an absent record, as the existing store API does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgressPair {
    pub story: Progression,
    pub character: Option<CharacterProgress>,
}

/// Only an acknowledged `Applied` or `AlreadyApplied` authorizes gameplay to continue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SceneCommit {
    Applied,
    AlreadyApplied,
}

/// Private on-disk envelope: callers cannot accidentally drop or change durable receipts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CharacterRecord {
    #[serde(flatten)]
    pub progress: CharacterProgress,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    scene_receipts: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    batch_receipts: BTreeMap<String, batch::BatchReceipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    checkpoint: Option<SceneCheckpoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reservation: Option<SceneReservation>,
    /// The prepared scene's complete membership, copied to every reserved record so a
    /// disconnected guest can find the owner without an account scan. Active records cannot be rekeyed by
    /// trash restoration, so these ids never need to be guessed or partially rewritten.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scene_roster: Option<Vec<batch::RosterMember>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) field_visit: Option<super::field_visit::FieldVisitV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) last_visit_commit: Option<super::field_visit::VisitReceipt>,
}

impl CharacterRecord {
    pub(crate) fn read(path: &Path) -> Result<Option<Self>> {
        let Some(bytes) = fsio::read_capped(path, MAX_CHARACTER_RECORD_BYTES)? else { return Ok(None) };
        fsio::check_schema_version_upto(path, &bytes, MAX_CHARACTER_PROGRESS_SCHEMA_VERSION)?;
        let record: Self = serde_json::from_slice(&bytes)
            .map_err(|e| PersistError::corrupt(path, format!("invalid character record: {e}")))?;
        record.validate().map_err(|e| PersistError::corrupt(path, e.to_string()))?;
        Ok(Some(record))
    }

    pub(super) fn validate(&self) -> Result<()> {
        self.progress.validate()?;
        if self.progress.schema_version == 0 || self.progress.schema_version > MAX_CHARACTER_PROGRESS_SCHEMA_VERSION {
            return Err(invalid("unsupported character schema"));
        }
        let version = self.metadata_version();
        if self.progress.schema_version != version {
            return Err(invalid("character schema does not match its durable metadata"));
        }
        if self.scene_receipts.len() > MAX_SCENE_RECEIPTS || self.scene_receipts.iter().any(|(k, v)| !is_digest(k) || !is_digest(v)) {
            return Err(invalid("invalid or oversized scene receipt set"));
        }
        if self.field_visit.is_some() != self.last_visit_commit.is_some() {
            return Err(invalid("a field visit and its retained operation receipt must be present together"));
        }
        super::field_visit::validate_link(self.field_visit.as_ref(), self.checkpoint.as_ref(), self.reservation.as_ref())?;
        if let Some(receipt) = &self.last_visit_commit { receipt.validate()?; }
        batch::validate_record(self)?;
        Ok(())
    }

    fn metadata_version(&self) -> u32 {
        if self.field_visit.as_ref().is_some_and(super::field_visit::FieldVisitV1::requires_large_format) {
            MAX_CHARACTER_PROGRESS_SCHEMA_VERSION
        } else if self.field_visit.is_some() || self.last_visit_commit.is_some() {
            CHARACTER_VISIT_VERSION
        } else if !self.batch_receipts.is_empty() || self.has_continuation() || self.scene_roster.is_some() {
            CHARACTER_CONTINUATION_VERSION
        } else if self.scene_receipts.is_empty() { SCHEMA_VERSION } else { CHARACTER_RECEIPT_VERSION }
    }

    pub(super) fn stamp(&mut self) { self.progress.schema_version = self.metadata_version(); }

    /// The store-wide writer fence needed by this already validated envelope. Recovery
    /// considers both images so a large-to-small transition cannot lower its requirement.
    pub(crate) fn minimum_index_version(&self) -> u32 {
        match self.progress.schema_version {
            MAX_CHARACTER_PROGRESS_SCHEMA_VERSION.. => crate::index::LARGE_FIELD_VISIT_INDEX_VERSION,
            CHARACTER_VISIT_VERSION => crate::index::FIELD_VISIT_INDEX_VERSION,
            CHARACTER_RECEIPT_VERSION | CHARACTER_CONTINUATION_VERSION => crate::index::DURABLE_STORY_INDEX_VERSION,
            _ => crate::model::INDEX_LOG_VERSION,
        }
    }

    pub(crate) fn has_continuation(&self) -> bool { self.checkpoint.is_some() || self.reservation.is_some() }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Images {
    pub(super) story: Option<Progression>,
    pub(super) character: Option<CharacterRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    account: AccountId,
    game: GameId,
    receipt: String,
    result: String,
    before: Images,
    after: Images,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IntentFile {
    schema_version: u32,
    kind: String,
    checksum: String,
    intent: Intent,
}

fn invalid(reason: &str) -> PersistError {
    PersistError::InvalidField { field: "scene_transaction", reason: reason.into() }
}

fn validate_key(scene_key: &str, result: &[u8]) -> Result<()> {
    if scene_key.is_empty() || scene_key.len() > 256 || scene_key.chars().any(char::is_control) {
        return Err(invalid("scene key must be nonempty, bounded and have no control characters"));
    }
    if result.len() > MAX_BLOB_BYTES { return Err(invalid("selected result exceeds the blob limit")) }
    Ok(())
}

fn receipt_key(scene_key: &str, story: &Progression) -> Result<String> {
    if story.blob.len() > MAX_BLOB_BYTES || story.schema_version != SCHEMA_VERSION { return Err(invalid("invalid story precondition")) }
    // Account isolation is enforced by the enclosing record; excluding account id keeps
    // receipts meaningful after the store's explicit soft-delete/restore rekeys an account.
    digest(&("fmv-scene-receipt-v1", scene_key, story.game, &story.blob))
}

fn result_key(selected_result: &[u8]) -> Result<String> {
    digest(&("fmv-scene-selected-result-v1", selected_result))
}

pub(super) fn is_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub(super) fn digest<T: Serialize>(value: &T) -> Result<String> {
    let bytes = serde_json::to_vec(value).map_err(|e| invalid(&format!("cannot serialize transaction: {e}")))?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

pub(super) fn encoded<T: Serialize>(path: &Path, value: &T, max: u64) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|e| invalid(&format!("cannot serialize record: {e}")))?;
    bytes.push(b'\n');
    if bytes.len() as u64 > max {
        return Err(PersistError::TooLarge { path: path.to_path_buf(), size: bytes.len() as u64, max });
    }
    Ok(bytes)
}

impl ProgressPair {
    pub(super) fn validate(&self, account: AccountId, game: GameId) -> Result<()> {
        if self.story.account_id != account || self.story.game != game || self.story.schema_version != SCHEMA_VERSION {
            return Err(invalid("story belongs to a different account/game or schema"));
        }
        if self.story.blob.len() > MAX_BLOB_BYTES {
            return Err(PersistError::BlobTooLarge { len: self.story.blob.len(), max: MAX_BLOB_BYTES });
        }
        if let Some(c) = &self.character {
            if c.account_id != account || c.game != game || c.schema_version == 0 || c.schema_version > MAX_CHARACTER_PROGRESS_SCHEMA_VERSION {
                return Err(invalid("character belongs to a different account/game or schema"));
            }
            c.validate()?;
        }
        Ok(())
    }
}

impl Images {
    pub(super) fn minimum_index_version(&self) -> u32 {
        self.character.as_ref().map_or(crate::model::INDEX_LOG_VERSION, CharacterRecord::minimum_index_version)
    }

    pub(super) fn pair(&self, account: AccountId, game: GameId) -> ProgressPair {
        ProgressPair { story: self.story.clone().unwrap_or_else(|| Progression::empty(account, game)), character: self.character.as_ref().map(|r| r.progress.clone()) }
    }
}

impl Store {
    /// Read both records while excluding progress writers. Two separate getter calls cannot
    /// offer this guarantee: a completed transaction could occur between those calls.
    pub fn get_progress_pair(&self, account: AccountId, game: GameId) -> Result<ProgressPair> {
        let _read = self.progress_read()?;
        if !self.index().contains_account(account.0) { return Err(PersistError::UnknownAccount(account)) }
        Ok(self.read_images(account, game)?.pair(account, game))
    }

    pub(super) fn read_images(&self, account: AccountId, game: GameId) -> Result<Images> {
        let story_path = self.progression_path(account.0, game)?;
        let story: Option<Progression> = fsio::read_json(&story_path, MAX_RECORD_BYTES)?;
        let character = CharacterRecord::read(&self.character_path(account.0, game)?)?;
        let images = Images { story, character };
        images.pair(account, game).validate(account, game).map_err(|e| PersistError::corrupt(&story_path, e.to_string()))?;
        Ok(images)
    }

    /// Store ordinary character progress, retaining all existing scene receipts. The writer
    /// and progress write locks must already be held. A corrupt prior envelope is not replaced.
    /// Any uncertain replacement fences the Store before a caller can retry a resource delta.
    pub(super) fn write_character_record(&self, counters: &super::Counters, path: &Path, progress: CharacterProgress) -> Result<CharacterProgress> {
        self.write_character_record_with_writer(counters, path, progress, fsio::atomic_write)
    }

    fn write_character_record_with_writer(
        &self,
        counters: &super::Counters,
        path: &Path,
        progress: CharacterProgress,
        write: impl FnOnce(&Path, &[u8]) -> Result<()>,
    ) -> Result<CharacterProgress> {
        let mut stored = match CharacterRecord::read(path)? {
            Some(record) if record.has_continuation() => return Err(PersistError::TransactionConflict("character has an active scene continuation".into())),
            Some(mut record) => { record.progress = progress; record },
            None => CharacterRecord { progress, scene_receipts: BTreeMap::new(), batch_receipts: BTreeMap::new(), checkpoint: None, reservation: None, scene_roster: None, field_visit: None, last_visit_commit: None },
        };
        stored.stamp();
        stored.validate()?;
        // Refusals of shape or size happen before the uncertain-write fence. The exact
        // bytes validated here are the bytes replaced; a post-rename sync failure must not
        // let the caller re-read the debited record and apply the same delta again.
        let bytes = encoded(path, &stored, MAX_CHARACTER_RECORD_BYTES)?;
        if stored.minimum_index_version() >= crate::index::LARGE_FIELD_VISIT_INDEX_VERSION {
            self.ensure_large_field_visit_format(counters)?;
        }
        self.transaction_poisoned.store(true, Ordering::Release);
        write(path, &bytes)?;
        self.transaction_poisoned.store(false, Ordering::Release);
        Ok(stored.progress)
    }

    /// Check a durable receipt before reconstructing a debit after a retry or restart.
    /// `selected_result` is a stable, adapter-defined encoding of the selected story outcome
    /// and resource delta. A different result for the same scene and prior story is a conflict.
    /// This does not infer success from the current story or gil balance.
    /// A matching receipt establishes the store compatibility fence before authorizing an
    /// acknowledgment. A missing or conflicting receipt changes neither records nor format.
    pub fn scene_progress_applied(&self, scene_key: &str, expected_story: &Progression, selected_result: &[u8]) -> Result<bool> {
        validate_key(scene_key, selected_result)?;
        let writer = self.writer()?;
        let _progress = self.progress_write();
        let (account, game) = (expected_story.account_id, expected_story.game);
        if !self.index().contains_account(account.0) { return Err(PersistError::UnknownAccount(account)) }
        let receipt = receipt_key(scene_key, expected_story)?;
        let result = result_key(selected_result)?;
        let current = self.read_images(account, game)?;
        if current.character.as_ref().is_some_and(CharacterRecord::has_continuation) {
            return Err(PersistError::TransactionConflict("account has an active scene continuation".into()));
        }
        match current.character.as_ref().and_then(|c| c.scene_receipts.get(&receipt)) {
            None => Ok(false),
            Some(recorded) if recorded == &result => {
                self.ensure_index_format(&writer, crate::index::DURABLE_STORY_INDEX_VERSION.max(current.minimum_index_version()))?;
                Ok(true)
            }
            Some(_) => Err(PersistError::TransactionConflict("receipt already records a different result".into())),
        }
    }

    /// As [`Store::commit_scene_progress`], using an explicit stable outcome encoding for
    /// retries that reconstruct current character progress. Use the same encoding with
    /// [`Store::scene_progress_applied`] before calculating another debit. Game rules must
    /// validate that these exact after-images implement `selected_result`.
    pub fn commit_scene_progress_with_result(&self, scene_key: &str, selected_result: &[u8], expected: &ProgressPair, next: &ProgressPair) -> Result<SceneCommit> {
        validate_key(scene_key, selected_result)?;
        self.commit_scene_inner(scene_key, result_key(selected_result)?, expected, next, |_| Ok(()))
    }

    /// Commit a validated scene's exact story and character after-images once. The caller
    /// computes game rules (including sufficient funds) from `expected`, then submits this
    /// compare-and-swap. A stale pair changes nothing. `scene_key` must identify the authored
    /// scene/action; the store combines it with the exact prior story blob for a receipt.
    ///
    /// A duplicate with the same result is a no-op even if later rewards changed the current
    /// record. Reusing the receipt with another result conflicts. A receipt is never inferred
    /// from a story value and never discarded to make space. Both pairs must contain character
    /// progress; callers must persist verified game defaults before attempting a paid scene.
    /// The result hash excludes only write timestamps and envelope schema, so a retry may use
    /// a new timestamp without changing its selected outcome.
    pub fn commit_scene_progress(&self, scene_key: &str, expected: &ProgressPair, next: &ProgressPair) -> Result<SceneCommit> {
        self.commit_scene_with_hook(scene_key, expected, next, |_| Ok(()))
    }

    fn commit_scene_with_hook(&self, scene_key: &str, expected: &ProgressPair, next: &ProgressPair, hook: impl FnMut(u8) -> Result<()>) -> Result<SceneCommit> {
        let mut result_story = next.story.clone();
        result_story.updated_at = 0;
        result_story.account_id = AccountId(0);
        let mut result_character = next.character.clone();
        if let Some(c) = &mut result_character { c.updated_at = 0; c.schema_version = SCHEMA_VERSION; c.account_id = AccountId(0); }
        let result = digest(&("fmv-scene-result-v1", result_story, result_character))?;
        self.commit_scene_inner(scene_key, result, expected, next, hook)
    }

    fn commit_scene_inner(&self, scene_key: &str, result: String, expected: &ProgressPair, next: &ProgressPair, mut hook: impl FnMut(u8) -> Result<()>) -> Result<SceneCommit> {
        validate_key(scene_key, &[])?;
        let (account, game) = (expected.story.account_id, expected.story.game);
        expected.validate(account, game)?;
        next.validate(account, game)?;
        if expected.character.is_none() || next.character.is_none() { return Err(invalid("scene transaction requires stored character progress")) }
        let receipt = receipt_key(scene_key, &expected.story)?;

        let _writer = self.writer()?;
        let _progress = self.progress_write();
        if !self.index().contains_account(account.0) { return Err(PersistError::UnknownAccount(account)) }
        let before = self.read_images(account, game)?;
        if before.character.as_ref().is_some_and(CharacterRecord::has_continuation) {
            return Err(PersistError::TransactionConflict("account has an active scene continuation".into()));
        }
        if let Some(prior) = before.character.as_ref().and_then(|c| c.scene_receipts.get(&receipt)) {
            if prior != &result { return Err(PersistError::TransactionConflict("receipt already records a different result".into())); }
            self.ensure_index_format(&_writer, crate::index::DURABLE_STORY_INDEX_VERSION.max(before.minimum_index_version()))?;
            return Ok(SceneCommit::AlreadyApplied);
        }
        if before.pair(account, game) != *expected {
            return Err(PersistError::TransactionConflict("story or character precondition changed".into()));
        }
        let mut character = before.character.clone().ok_or_else(|| invalid("character progress is absent"))?;
        if character.scene_receipts.len() + character.batch_receipts.len() >= MAX_SCENE_RECEIPTS { return Err(invalid("scene receipt capacity exhausted; receipts cannot be evicted")) }
        character.progress = next.character.clone().ok_or_else(|| invalid("character after-image is absent"))?;
        character.scene_receipts.insert(receipt.clone(), result.clone());
        character.stamp();
        character.validate()?;
        let after = Images { story: Some(next.story.clone()), character: Some(character) };
        let intent = Intent { account, game, receipt, result, before, after };
        self.validate_intent(&intent)?;
        // Serialize every future write before attempting the durable intent; no size/shape
        // error may strand an otherwise valid store after the commit point.
        self.encode_after(&intent)?;
        let file = IntentFile { schema_version: INTENT_VERSION, kind: "fmv-scene-transaction".into(), checksum: digest(&intent)?, intent };
        let path = self.root.join(INTENT_FILE);
        let bytes = encoded(&path, &file, MAX_INTENT_BYTES)?;
        hook(0)?;
        self.ensure_index_format(&_writer, crate::index::DURABLE_STORY_INDEX_VERSION.max(file.intent.before.minimum_index_version()).max(file.intent.after.minimum_index_version()))?;
        self.transaction_poisoned.store(true, Ordering::Release);
        fsio::atomic_write(&path, &bytes)?;
        hook(1)?;
        self.install_after(&file.intent, &mut hook)?;
        std::fs::remove_file(&path).map_err(|e| PersistError::io("remove scene intent", &path, e))?;
        hook(4)?;
        fsio::sync_dir(&self.root).map_err(|e| PersistError::io("sync scene intent removal", &self.root, e))?;
        hook(5)?;
        self.transaction_poisoned.store(false, Ordering::Release);
        Ok(SceneCommit::Applied)
    }

    fn validate_intent(&self, intent: &Intent) -> Result<()> {
        if !is_digest(&intent.receipt) || !is_digest(&intent.result) { return Err(invalid("invalid receipt digest")) }
        intent.before.pair(intent.account, intent.game).validate(intent.account, intent.game)?;
        intent.after.pair(intent.account, intent.game).validate(intent.account, intent.game)?;
        let old = intent.before.character.as_ref().ok_or_else(|| invalid("intent has no character before-image"))?;
        let new = intent.after.character.as_ref().ok_or_else(|| invalid("intent has no character after-image"))?;
        old.validate()?;
        new.validate()?;
        if intent.after.story.is_none() { return Err(invalid("intent has no story after-image")) }
        let mut receipts = old.scene_receipts.clone();
        if receipts.insert(intent.receipt.clone(), intent.result.clone()).is_some() || receipts != new.scene_receipts {
            return Err(invalid("intent must append exactly its one new receipt"));
        }
        if old.has_continuation() || new.has_continuation() || old.batch_receipts != new.batch_receipts || old.scene_roster != new.scene_roster
            || old.field_visit != new.field_visit || old.last_visit_commit != new.last_visit_commit {
            return Err(invalid("ordinary scene intent cannot change or bypass a continuation"));
        }
        Ok(())
    }

    fn encode_after(&self, intent: &Intent) -> Result<(Vec<u8>, Vec<u8>)> {
        let story = intent.after.story.as_ref().ok_or_else(|| invalid("missing story after-image"))?;
        let character = intent.after.character.as_ref().ok_or_else(|| invalid("missing character after-image"))?;
        Ok((encoded(&self.progression_path(intent.account.0, intent.game)?, story, MAX_RECORD_BYTES)?, encoded(&self.character_path(intent.account.0, intent.game)?, character, MAX_CHARACTER_RECORD_BYTES)?))
    }

    fn install_after(&self, intent: &Intent, hook: &mut impl FnMut(u8) -> Result<()>) -> Result<()> {
        let (story, character) = self.encode_after(intent)?;
        fsio::ensure_dir(&self.progression_dir(intent.account.0))?;
        fsio::atomic_write(&self.progression_path(intent.account.0, intent.game)?, &story)?;
        hook(2)?;
        fsio::atomic_write(&self.character_path(intent.account.0, intent.game)?, &character)?;
        hook(3)
    }

    pub(super) fn recover_scene_progress(&self) -> Result<()> {
        let path = self.root.join(INTENT_FILE);
        let Some(bytes) = fsio::read_capped(&path, batch::MAX_BATCH_INTENT_BYTES)? else { return Ok(()) };
        fsio::check_schema_version_upto(&path, &bytes, super::field_visit::VISIT_INTENT_VERSION)?;
        let schema: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| PersistError::corrupt(&path, format!("invalid scene intent: {e}")))?;
        if schema.get("schema_version").and_then(serde_json::Value::as_u64) == Some(u64::from(super::field_visit::VISIT_INTENT_VERSION)) {
            return self.recover_field_visit(&path, &bytes);
        }
        if schema.get("schema_version").and_then(serde_json::Value::as_u64) == Some(u64::from(batch::BATCH_INTENT_VERSION)) {
            return self.recover_scene_batch(&path, &bytes);
        }
        if bytes.len() as u64 > MAX_INTENT_BYTES { return Err(PersistError::TooLarge { path, size: bytes.len() as u64, max: MAX_INTENT_BYTES }); }
        let file: IntentFile = serde_json::from_slice(&bytes).map_err(|e| PersistError::corrupt(&path, format!("invalid scene intent: {e}")))?;
        if file.kind != "fmv-scene-transaction" || file.checksum != digest(&file.intent)? { return Err(PersistError::corrupt(&path, "scene intent kind or checksum mismatch")) }
        let intent = &file.intent;
        let required = intent.before.minimum_index_version().max(intent.after.minimum_index_version());
        if required >= crate::index::FIELD_VISIT_INDEX_VERSION && self.index().format_version() < required {
            return Err(PersistError::corrupt(&path, "visit-bearing scene intent lacks its required index format fence"));
        }
        self.validate_intent(intent).map_err(|e| PersistError::corrupt(&path, e.to_string()))?;
        if !self.index().contains_account(intent.account.0) { return Err(PersistError::corrupt(&path, "scene intent account is not live")) }
        let current = self.read_images(intent.account, intent.game)?;
        if (current.story != intent.before.story && current.story != intent.after.story)
            || (current.character != intent.before.character && current.character != intent.after.character) {
            return Err(PersistError::corrupt(&path, "scene intent conflicts with unrelated progress; nothing overwritten"));
        }
        self.install_after(intent, &mut |_| Ok(()))?;
        std::fs::remove_file(&path).map_err(|e| PersistError::io("remove recovered scene intent", &path, e))?;
        fsio::sync_dir(&self.root).map_err(|e| PersistError::io("sync recovered scene intent", &self.root, e))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmv_core::InventoryEntry;
    use std::sync::{mpsc, Arc};
    use std::time::Duration;

    fn rig() -> (tempfile::TempDir, Store, ProgressPair, ProgressPair) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("state")).unwrap();
        let account = store.register("Scene", [1, 2, 3], "original", None, 1).unwrap().account_id;
        let mut character = CharacterProgress::new(account, GameId::Ff7, 0, 7, 2);
        character.gil = 1;
        character.exp = 626;
        character.ap = 9;
        character.limit = 17;
        character.inventory = Some(vec![InventoryEntry { item: 1, count: 3 }]);
        store.put_character_progress(&character).unwrap();
        store.put_progression(&Progression::new(account, GameId::Ff7, vec![1, 36, 0], 2)).unwrap();
        let before = store.get_progress_pair(account, GameId::Ff7).unwrap();
        let mut after = before.clone();
        after.story.blob = vec![3, 39, 0, 66, 1, 111, 1];
        after.story.updated_at = 3;
        let c = after.character.as_mut().unwrap();
        c.gil = 0;
        c.updated_at = 3;
        (dir, store, before, after)
    }

    fn expected_after(mut after: ProgressPair) -> ProgressPair {
        after.character.as_mut().unwrap().schema_version = CHARACTER_RECEIPT_VERSION;
        after
    }

    #[test]
    fn paired_scene_validation_precedes_the_fence_and_first_commit_protects_extended_story() {
        let (_dir, store, before, after) = rig();
        assert_eq!(store.index().format_version(), crate::model::INDEX_LOG_VERSION);
        let mut invalid = after.clone();
        invalid.story.account_id = AccountId(999);
        assert!(store.commit_scene_progress("first durable story", &before, &invalid).is_err());
        assert_eq!(store.index().format_version(), crate::model::INDEX_LOG_VERSION);
        assert_eq!(store.get_progress_pair(before.story.account_id, before.story.game).unwrap(), before);
        assert_eq!(store.commit_scene_progress("first durable story", &before, &after).unwrap(), SceneCommit::Applied);
        assert_eq!(store.index().format_version(), crate::index::DURABLE_STORY_INDEX_VERSION);
        assert_eq!(store.get_progress_pair(before.story.account_id, before.story.game).unwrap(), expected_after(after));
    }

    #[test]
    fn a_matching_legacy_scene_receipt_fastpath_establishes_the_fence_without_rewriting_progress() {
        let (dir, store, before, after) = rig();
        let account = before.story.account_id;
        let game = before.story.game;
        let key = "legacy completed scene";
        let selected = b"stable legacy outcome";
        let character_path = store.character_path(account.0, game).unwrap();
        let story_path = store.progression_path(account.0, game).unwrap();
        // Construct the exact receipt-only schema supported before store-wide fencing.
        // The input files are already committed; only a duplicate request arrives now.
        let mut character = CharacterRecord::read(&character_path).unwrap().unwrap();
        character.progress = after.character.clone().unwrap();
        character.scene_receipts.insert(receipt_key(key, &before.story).unwrap(), result_key(selected).unwrap());
        character.stamp();
        character.validate().unwrap();
        fsio::write_json(&character_path, &character, MAX_CHARACTER_RECORD_BYTES).unwrap();
        fsio::write_json(&story_path, &after.story, MAX_RECORD_BYTES).unwrap();
        let bytes = (std::fs::read(&character_path).unwrap(), std::fs::read(&story_path).unwrap());
        assert_eq!(store.index().format_version(), crate::model::INDEX_LOG_VERSION);
        assert!(!store.scene_progress_applied("no matching receipt", &before.story, selected).unwrap());
        assert!(matches!(store.scene_progress_applied(key, &before.story, b"another result"), Err(PersistError::TransactionConflict(_))));
        assert_eq!(store.index().format_version(), crate::model::INDEX_LOG_VERSION, "false/conflicting lookup cannot opt into a format");
        assert!(store.scene_progress_applied(key, &before.story, selected).unwrap(), "the saver's public receipt fastpath");
        assert_eq!(store.index().format_version(), crate::index::DURABLE_STORY_INDEX_VERSION);
        assert_eq!(store.commit_scene_progress_with_result(key, selected, &before, &after).unwrap(), SceneCommit::AlreadyApplied);
        assert_eq!(store.index().format_version(), crate::index::DURABLE_STORY_INDEX_VERSION);
        assert_eq!((std::fs::read(&character_path).unwrap(), std::fs::read(&story_path).unwrap()), bytes);
        drop(store);
        let reopened = Store::open(dir.path().join("state")).unwrap();
        assert_eq!(reopened.index().format_version(), crate::index::DURABLE_STORY_INDEX_VERSION);
        assert_eq!(reopened.get_progress_pair(account, game).unwrap(), expected_after(after));
    }

    fn interrupted(stage: u8) -> PersistError {
        PersistError::Io { action: "injected crash", path: "scene".into(), source: std::io::Error::other(format!("stage {stage}")) }
    }

    #[test]
    fn a_scene_commits_both_records_once_preserves_inventory_and_rejects_another_answer() {
        let (_dir, store, before, after) = rig();
        assert_eq!(store.commit_scene_progress("flower", &before, &after).unwrap(), SceneCommit::Applied);
        assert_eq!(store.get_progress_pair(before.story.account_id, GameId::Ff7).unwrap(), expected_after(after.clone()));
        assert_eq!(store.commit_scene_progress("flower", &before, &after).unwrap(), SceneCommit::AlreadyApplied);
        let mut other = after.clone();
        other.character.as_mut().unwrap().gil = 1;
        assert!(matches!(store.commit_scene_progress("flower", &before, &other), Err(PersistError::TransactionConflict(_))));
        // A delayed retry cannot clobber a battle reward paid after this scene.
        store.update_character_progress(before.story.account_id, GameId::Ff7, |old| {
            let mut c = old.unwrap(); c.gil += 20; c
        }).unwrap();
        other = after.clone();
        other.story.updated_at = 20;
        other.character.as_mut().unwrap().updated_at = 20;
        assert_eq!(store.commit_scene_progress("flower", &before, &other).unwrap(), SceneCommit::AlreadyApplied);
        assert_eq!(store.get_character_progress(before.story.account_id, GameId::Ff7).unwrap().unwrap().gil, 20);
        assert!(!store.root.join(INTENT_FILE).exists());
    }

    #[test]
    fn a_stable_outcome_receipt_can_be_checked_before_reconstructing_a_debit() {
        let (_dir, store, before, after) = rig();
        let selected = b"story-after=39;flower=true;gil-debit=1";
        assert!(!store.scene_progress_applied("flower", &before.story, selected).unwrap());
        store.commit_scene_progress_with_result("flower", selected, &before, &after).unwrap();
        store.update_character_progress(before.story.account_id, GameId::Ff7, |old| {
            let mut c = old.unwrap(); c.gil = 50; c
        }).unwrap();
        assert!(store.scene_progress_applied("flower", &before.story, selected).unwrap());
        assert!(matches!(store.scene_progress_applied("flower", &before.story, b"different choice"), Err(PersistError::TransactionConflict(_))));
        let restored = store.restore_account(&store.delete_account(before.story.account_id, 10).unwrap().entry, 11).unwrap();
        let mut rekeyed = before.story;
        rekeyed.account_id = restored.account_id;
        assert!(store.scene_progress_applied("flower", &rekeyed, selected).unwrap());
        assert_eq!(store.get_character_progress(restored.account_id, GameId::Ff7).unwrap().unwrap().gil, 50);
    }

    #[test]
    fn no_purchase_is_also_a_once_only_complete_story_result() {
        let (_dir, store, before, mut after) = rig();
        after.character.as_mut().unwrap().gil = 1;
        after.story.blob = vec![3, 39, 0, 66, 1];
        assert_eq!(store.commit_scene_progress("flower", &before, &after).unwrap(), SceneCommit::Applied);
        assert_eq!(store.get_progress_pair(before.story.account_id, GameId::Ff7).unwrap(), expected_after(after));
    }

    #[test]
    fn stale_invalid_missing_or_oversized_preconditions_change_nothing() {
        let (_dir, store, before, after) = rig();
        let mut stale = before.clone();
        stale.character.as_mut().unwrap().gil += 1;
        assert!(matches!(store.commit_scene_progress("flower", &stale, &after), Err(PersistError::TransactionConflict(_))));
        let mut invalid = after.clone();
        invalid.character.as_mut().unwrap().level = 0;
        assert!(store.commit_scene_progress("flower", &before, &invalid).is_err());
        invalid = after.clone(); invalid.character = None;
        assert!(store.commit_scene_progress("flower", &before, &invalid).is_err());
        invalid = after; invalid.story.blob = vec![0; MAX_BLOB_BYTES + 1];
        assert!(store.commit_scene_progress("flower", &before, &invalid).is_err());
        assert_eq!(store.get_progress_pair(before.story.account_id, GameId::Ff7).unwrap(), before);
        assert!(!store.root.join(INTENT_FILE).exists());
    }

    #[test]
    fn every_durable_boundary_recovers_to_exactly_one_complete_result() {
        for crash_at in 0..=5 {
            let (dir, store, before, after) = rig();
            let root = store.root.clone();
            let result = store.commit_scene_with_hook("flower", &before, &after, |at| if at == crash_at { Err(interrupted(at)) } else { Ok(()) });
            assert!(result.is_err());
            if crash_at != 0 {
                assert!(matches!(store.get_progress_pair(before.story.account_id, GameId::Ff7), Err(PersistError::TransactionRecoveryRequired)));
                assert!(matches!(store.put_progression(&before.story), Err(PersistError::TransactionRecoveryRequired)));
                assert!(matches!(store.put_character_progress(before.character.as_ref().unwrap()), Err(PersistError::TransactionRecoveryRequired)));
                assert!(matches!(store.snapshot(dir.path().join("unsafe")), Err(PersistError::TransactionRecoveryRequired)));
                assert!(matches!(store.delete_account(before.story.account_id, 4), Err(PersistError::TransactionRecoveryRequired)));
            }
            drop(store);
            let reopened = Store::open(&root).unwrap();
            let actual = reopened.get_progress_pair(before.story.account_id, GameId::Ff7).unwrap();
            assert_eq!(actual, if crash_at == 0 { before.clone() } else { expected_after(after.clone()) }, "boundary {crash_at}");
            assert_eq!(reopened.commit_scene_progress("flower", &before, &after).unwrap(), if crash_at == 0 { SceneCommit::Applied } else { SceneCommit::AlreadyApplied });
            assert_eq!(reopened.get_character_progress(before.story.account_id, GameId::Ff7).unwrap().unwrap().gil, 0);
        }
    }

    #[test]
    fn a_corrupt_future_or_conflicting_intent_never_overwrites_unrelated_data() {
        for fault in 0..4 {
            let (_dir, store, before, after) = rig();
            let root = store.root.clone();
            store.commit_scene_with_hook("flower", &before, &after, |at| if at == 1 { Err(interrupted(at)) } else { Ok(()) }).unwrap_err();
            drop(store);
            let path = root.join(INTENT_FILE);
            let mut intent: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            match fault {
                0 => intent["checksum"] = "0".repeat(64).into(),
                1 => intent["schema_version"] = 2.into(),
                2 => intent["new_semantics"] = true.into(),
                _ => {
                    let mut unrelated = before.story.clone(); unrelated.blob = vec![8, 8];
                    fsio::write_json(&root.join("progression/1/Ff7.json"), &unrelated, MAX_RECORD_BYTES).unwrap();
                }
            }
            std::fs::write(&path, serde_json::to_vec(&intent).unwrap()).unwrap();
            let story = std::fs::read(root.join("progression/1/Ff7.json")).unwrap();
            let character = std::fs::read(root.join("progression/1/Ff7.character.json")).unwrap();
            assert!(Store::open(&root).is_err(), "fault {fault}");
            assert_eq!(std::fs::read(root.join("progression/1/Ff7.json")).unwrap(), story);
            assert_eq!(std::fs::read(root.join("progression/1/Ff7.character.json")).unwrap(), character);
            assert!(path.exists());
        }
    }

    #[test]
    fn paired_read_reward_and_snapshot_wait_for_the_complete_commit() {
        let (dir, store, before, after) = rig();
        let store = Arc::new(store);
        let (at_intent_tx, at_intent_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let writer = Arc::clone(&store);
        let b = before.clone(); let a = after.clone();
        let commit = std::thread::spawn(move || writer.commit_scene_with_hook("flower", &b, &a, |stage| {
            if stage == 2 { at_intent_tx.send(()).unwrap(); release_rx.recv().unwrap(); }
            Ok(())
        }));
        at_intent_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let account = before.story.account_id;
        let reader = Arc::clone(&store);
        let (read_tx, read_rx) = mpsc::channel();
        let read = std::thread::spawn(move || read_tx.send(reader.get_progress_pair(account, GameId::Ff7)).unwrap());
        assert!(read_rx.recv_timeout(Duration::from_millis(50)).is_err(), "reader must not observe a mixed pair");
        let snapshot_path = dir.path().join("snapshot");
        let snapshot_store = Arc::clone(&store); let dest = snapshot_path.clone();
        let snap = std::thread::spawn(move || snapshot_store.snapshot(dest));
        let rewards = Arc::clone(&store);
        let reward = std::thread::spawn(move || rewards.update_character_progress(account, GameId::Ff7, |old| {
            let mut c = old.unwrap(); c.gil += 5; c
        }));
        release_tx.send(()).unwrap();
        assert_eq!(commit.join().unwrap().unwrap(), SceneCommit::Applied);
        let observed = read_rx.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
        assert_eq!(observed.story, after.story);
        assert!([0, 5].contains(&observed.character.unwrap().gil));
        read.join().unwrap(); reward.join().unwrap().unwrap(); snap.join().unwrap().unwrap();
        assert_eq!(store.get_character_progress(account, GameId::Ff7).unwrap().unwrap().gil, 5);
        let snapshot = Store::open(snapshot_path).unwrap();
        let observed = snapshot.get_progress_pair(account, GameId::Ff7).unwrap();
        assert_eq!(observed.story, after.story);
        assert!([0, 5].contains(&observed.character.unwrap().gil));
    }

    #[test]
    fn ordinary_replacement_and_trash_restore_preserve_receipts() {
        let (_dir, store, before, after) = rig();
        store.commit_scene_progress("flower", &before, &after).unwrap();
        // The caller's old API value has no receipts; replacement must still carry them.
        let mut replacement = after.character.clone().unwrap(); replacement.gil = 10;
        store.put_character_progress(&replacement).unwrap();
        assert_eq!(store.commit_scene_progress("flower", &before, &after).unwrap(), SceneCommit::AlreadyApplied);
        let deleted = store.delete_account(before.story.account_id, 10).unwrap();
        let restored = store.restore_account(&deleted.entry, 11).unwrap();
        let path = store.character_path(restored.account_id.0, GameId::Ff7).unwrap();
        let record = CharacterRecord::read(&path).unwrap().unwrap();
        assert_eq!(record.scene_receipts.len(), 1);
        assert_eq!(record.progress.account_id, restored.account_id);
        assert_eq!(record.progress.gil, 10);
    }

    #[test]
    fn a_receipt_schema_without_its_receipts_cannot_be_read_or_downgraded() {
        let (_dir, store, before, after) = rig();
        store.commit_scene_progress("flower", &before, &after).unwrap();
        let path = store.character_path(before.story.account_id.0, GameId::Ff7).unwrap();
        for remove in [false, true] {
            let mut broken = serde_json::to_value(expected_after(after.clone()).character.unwrap()).unwrap();
            if !remove { broken["scene_receipts"] = serde_json::json!({}); }
            std::fs::write(&path, serde_json::to_vec(&broken).unwrap()).unwrap();
            assert!(matches!(store.get_character_progress(before.story.account_id, GameId::Ff7), Err(PersistError::Corrupt { .. })));
            assert!(matches!(store.put_character_progress(after.character.as_ref().unwrap()), Err(PersistError::Corrupt { .. })));
            assert_eq!(serde_json::from_slice::<serde_json::Value>(&std::fs::read(&path).unwrap()).unwrap(), broken);
        }
    }

    #[test]
    fn receipt_capacity_is_a_refusal_not_eviction_and_future_character_schema_is_rejected() {
        let (_dir, store, before, after) = rig();
        let path = store.character_path(before.story.account_id.0, GameId::Ff7).unwrap();
        let mut record = CharacterRecord::read(&path).unwrap().unwrap();
        for n in 0..MAX_SCENE_RECEIPTS { record.scene_receipts.insert(format!("{n:064x}"), "0".repeat(64)); }
        record.stamp();
        fsio::write_json(&path, &record, MAX_CHARACTER_RECORD_BYTES).unwrap();
        let current = store.get_progress_pair(before.story.account_id, GameId::Ff7).unwrap();
        assert!(store.commit_scene_progress("flower", &current, &after).is_err());
        assert_eq!(CharacterRecord::read(&path).unwrap().unwrap().scene_receipts.len(), MAX_SCENE_RECEIPTS);
        assert!(!store.root.join(INTENT_FILE).exists());
        record.progress.schema_version = MAX_CHARACTER_PROGRESS_SCHEMA_VERSION + 1;
        fsio::write_json(&path, &record, MAX_CHARACTER_RECORD_BYTES).unwrap();
        assert!(matches!(store.get_character_progress(before.story.account_id, GameId::Ff7), Err(PersistError::FutureSchema { .. })));
    }
}
