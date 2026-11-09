//! Error type for the persistence layer.
//!
//! Every failure mode, including corrupt or hostile files on disk, is reported through
//! [`PersistError`]; nothing in this crate panics on bad input.

use std::io;
use std::path::{Path, PathBuf};

use fmv_core::AccountId;
use thiserror::Error;

use crate::model::AvatarId;

pub type Result<T, E = PersistError> = std::result::Result<T, E>;

/// Callers should keep a catch-all arm when matching: variants are added as the store grows.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PersistError {
    /// An operating-system failure, with what we were doing and to which path.
    #[error("{action} {}: {source}", .path.display())]
    Io {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// A file exists but is not a valid record of the expected kind.
    #[error("corrupt store file {}: {reason}", .path.display())]
    Corrupt { path: PathBuf, reason: String },

    /// The record was written by a newer build than this one.
    #[error(
        "{} has schema_version {found}, newer than the {supported} this build understands; \
         refusing to load it (upgrade the server, or restore a snapshot taken before the upgrade)",
        .path.display()
    )]
    FutureSchema {
        path: PathBuf,
        found: u32,
        supported: u32,
    },

    /// `schema_version` 0 never existed.
    #[error(
        "{} has invalid schema_version {found} (valid versions are 1..={supported})",
        .path.display()
    )]
    InvalidSchema {
        path: PathBuf,
        found: u32,
        supported: u32,
    },

    #[error("{} is {size} bytes, over the {max} byte limit", .path.display())]
    TooLarge { path: PathBuf, size: u64, max: u64 },

    /// Another live process owns the state directory.
    #[error("state directory is locked via {} by pid {pid}: {detail}", .path.display())]
    Locked {
        path: PathBuf,
        pid: u32,
        detail: String,
    },

    #[error(
        "lock file {} is unreadable or malformed ({detail}); if no server is using this \
         state directory, remove the file by hand",
        .path.display()
    )]
    LockCorrupt { path: PathBuf, detail: String },

    /// Records exist but the index (`index.log`, or a pre-v2 `index.json`) does not. Opening
    /// would reuse ids over live records.
    #[error(
        "state directory {} has records but no index (index.log); refusing to open it so \
         existing records are not overwritten (restore index.log or a snapshot)",
        .root.display()
    )]
    IndexMissing { root: PathBuf },

    /// The display name cannot be registered: nothing visible is left after cleaning it, or
    /// it mixes writing systems (see `fmv_core::AvatarAppearance::validate_new_name`, which
    /// says which).
    #[error(
        "display name is empty or not allowed after cleaning (it needs visible characters \
         from a single writing system)"
    )]
    InvalidName,

    #[error("display name {0:?} is already taken")]
    NameTaken(String),

    /// Unknown, malformed or oversized token. Deliberately does not say which.
    #[error("invalid or unknown token")]
    InvalidToken,

    #[error("unknown account {0:?}")]
    UnknownAccount(AccountId),

    #[error("unknown avatar {0}")]
    UnknownAvatar(AvatarId),

    #[error("account {0:?} already has an avatar")]
    AvatarExists(AccountId),

    #[error("invalid {field}: {reason}")]
    InvalidField { field: &'static str, reason: String },

    #[error("progression blob is {len} bytes, over the {max} byte limit")]
    BlobTooLarge { len: usize, max: usize },

    /// OS randomness failed (or produced something we refuse to use as a secret).
    #[error("OS randomness unavailable: {0}")]
    Entropy(String),

    #[error("id space exhausted")]
    IdExhausted,

    /// The store already holds the configured maximum number of accounts
    /// ([`StoreOptions::max_accounts`](crate::StoreOptions)). Only registration is refused;
    /// logins, avatars and progression are unaffected, and the limit is checked before any
    /// disk work, so a flood of refused registrations costs the store nothing.
    #[error(
        "the account limit of {max} has been reached; no new account can be registered \
         (existing accounts are not affected)"
    )]
    AccountLimitReached { max: u64 },

    /// `release_unclaimed` was asked to remove an account that is not an orphan.
    #[error("account {account:?} cannot be released: {reason}")]
    NotReleasable {
        account: AccountId,
        reason: &'static str,
    },

    /// A write to `index.log` failed and could not be rolled back, so appending more would
    /// make the file disagree with memory. Logins keep working; registration is refused until
    /// the server restarts (opening the store repairs the log from what is on disk).
    #[error(
        "the account index could not be written and is not safe to append to ({detail}); \
         restart the server so the log is repaired"
    )]
    IndexPoisoned { detail: String },

    #[error("snapshot: {0}")]
    Snapshot(String),

    /// `restore_account` was given a trash entry that is not there, or is not one that can be restored (a
    /// delete or a restore still in flight, or an entry that was restored already).
    #[error("no restorable trash entry {0:?}")]
    NoSuchTrashEntry(String),
}

impl PersistError {
    pub(crate) fn io(action: &'static str, path: &Path, source: io::Error) -> Self {
        PersistError::Io {
            action,
            path: path.to_path_buf(),
            source,
        }
    }

    pub(crate) fn corrupt(path: &Path, reason: impl Into<String>) -> Self {
        PersistError::Corrupt {
            path: path.to_path_buf(),
            reason: reason.into(),
        }
    }
}
