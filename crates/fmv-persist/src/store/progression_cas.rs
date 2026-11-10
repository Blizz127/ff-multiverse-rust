//! Exact ordinary story replacement, without permanent once-only scene receipts.
//!
//! A story-only A -> B -> A -> B sequence is legitimate. Comparing exact predecessor bytes
//! avoids lossy merging, while treating an already present B as success permits retry of an
//! uncertain delivery. No resource delta is reconstructed, and character metadata is untouched.

use std::path::Path;
use std::sync::atomic::Ordering;

use fmv_core::{AccountId, GameId};

use super::Store;
use crate::{fsio, PersistError, ProgressPair, Result, SceneCommit};
use crate::model::{MAX_BLOB_BYTES, MAX_RECORD_BYTES, SCHEMA_VERSION};

impl Store {
    /// Replace only this account's story when its bytes equal `expected_blob`. If the exact
    /// desired bytes are already present, return `AlreadyApplied` and the coherent current
    /// pair without changing either record. Another predecessor refuses the operation.
    ///
    /// No receipt is retained: ordinary story values can repeat, and this operation changes
    /// no resources. The caller validates the game transition and holds gameplay until this
    /// durable acknowledgment. A failed write fences all progress access until reopen; a
    /// single atomic replacement then recovers as either its old or its new complete record.
    /// This explicit exact-write API upgrades the store's recognized compatibility fence,
    /// preventing earlier writers from applying legacy story merge rules to the new data.
    pub fn compare_exchange_progression(
        &self,
        account: AccountId,
        game: GameId,
        expected_blob: &[u8],
        next_blob: &[u8],
        now: u64,
    ) -> Result<(SceneCommit, ProgressPair)> {
        self.compare_exchange_progression_with_writer(account, game, expected_blob, next_blob, now, fsio::atomic_write)
    }

    #[allow(clippy::too_many_arguments)]
    fn compare_exchange_progression_with_writer(
        &self,
        account: AccountId,
        game: GameId,
        expected_blob: &[u8],
        next_blob: &[u8],
        now: u64,
        write: impl FnOnce(&Path, &[u8]) -> Result<()>,
    ) -> Result<(SceneCommit, ProgressPair)> {
        for blob in [expected_blob, next_blob] {
            if blob.len() > MAX_BLOB_BYTES {
                return Err(PersistError::BlobTooLarge { len: blob.len(), max: MAX_BLOB_BYTES });
            }
        }
        let path = self.progression_path(account.0, game)?;
        let writer = self.writer()?;
        let _progress = self.progress_write();
        if !self.index().contains_account(account.0) { return Err(PersistError::UnknownAccount(account)); }
        self.ensure_no_scene_continuation(account, game)?;
        let mut current = ProgressPair {
            story: self.read_progression(account, game)?,
            character: self.read_character_progress(account, game)?,
        };
        if current.story.blob == next_blob {
            // A previously imported or directly stored v3 blob may predate this fence.
            // A successful exact-write acknowledgment must protect it from older writers.
            self.ensure_scene_batch_format(&writer)?;
            return Ok((SceneCommit::AlreadyApplied, current));
        }
        if current.story.blob != expected_blob {
            return Err(PersistError::TransactionConflict("ordinary story predecessor changed".into()));
        }
        current.story.blob = next_blob.to_vec();
        current.story.updated_at = now;
        current.story.schema_version = SCHEMA_VERSION;
        let mut bytes = serde_json::to_vec_pretty(&current.story)
            .map_err(|_| PersistError::InvalidField { field: "progression", reason: "cannot serialize exact story replacement".into() })?;
        bytes.push(b'\n');
        if bytes.len() as u64 > MAX_RECORD_BYTES {
            return Err(PersistError::TooLarge { path, size: bytes.len() as u64, max: MAX_RECORD_BYTES });
        }
        self.ensure_scene_batch_format(&writer)?;
        fsio::ensure_dir(&self.progression_dir(account.0))?;
        self.transaction_poisoned.store(true, Ordering::Release);
        write(&path, &bytes)?;
        self.transaction_poisoned.store(false, Ordering::Release);
        Ok((SceneCommit::Applied, current))
    }
}

#[cfg(test)]
mod tests;
