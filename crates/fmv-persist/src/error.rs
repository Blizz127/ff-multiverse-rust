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

#[derive(Debug, Error)]
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

    /// Records exist but `index.json` does not. Opening would reuse ids over live records.
    #[error(
        "state directory {} has records but no index.json; refusing to open it so existing \
         records are not overwritten (restore index.json or a snapshot)",
        .root.display()
    )]
    IndexMissing { root: PathBuf },

    #[error("display name is empty after sanitizing")]
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

    #[error("snapshot: {0}")]
    Snapshot(String),
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
