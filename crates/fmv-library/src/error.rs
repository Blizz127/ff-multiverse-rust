//! Error type for the owned-content library.

use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum LibraryError {
    #[error("cannot determine a state directory: pass one explicitly, set FMV_STATE_DIR, or provide a user data directory")]
    NoStateDir,
    #[error("no library at {0} (nothing has been imported there yet)")]
    NoLibrary(PathBuf),
    #[error("I/O error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// A read from an import source failed. The inner error is preserved so callers can
    /// downcast it (the importer uses this to recognise skippable disc sectors).
    #[error("error while reading the import source: {0}")]
    SourceRead(#[source] std::io::Error),
    #[error("invalid logical path {path:?}: {reason}")]
    BadLogicalPath { path: String, reason: &'static str },
    #[error("invalid set id {0:?}")]
    BadSetId(String),
    #[error("malformed blake3 digest {0:?}")]
    BadDigest(String),
    #[error("source {0} is inside the library directory; refusing to import library content into itself")]
    SourceInsideLibrary(PathBuf),
    #[error("source {0} is not a regular file")]
    NotRegularFile(PathBuf),
    #[error("{what} is too large: {size} bytes exceeds the limit of {limit} bytes")]
    TooLarge { what: String, size: u64, limit: u64 },
    #[error("source {0} changed while it was being imported (size, mtime or identity differs); import of this file aborted")]
    SourceChanged(PathBuf),
    #[error("size mismatch for {logical_path}: expected {expected} bytes but read {actual}")]
    SizeMismatch {
        logical_path: String,
        expected: u64,
        actual: u64,
    },
    #[error(
        "set {set_id} holds [{existing}]; refusing to add content that belongs to [{incoming}]"
    )]
    IncompatibleSet {
        set_id: String,
        existing: String,
        incoming: String,
    },
    #[error("library set {0} not found")]
    SetNotFound(String),
    #[error("entry {logical_path} not found in set {set_id}")]
    EntryNotFound {
        set_id: String,
        logical_path: String,
    },
    #[error(
        "set {set_id} already has {logical_path} with different content (existing blake3 {existing}, new blake3 {incoming}); \
         refusing to overwrite (replacing entries is not supported yet)"
    )]
    EntryConflict {
        set_id: String,
        logical_path: String,
        existing: String,
        incoming: String,
    },
    #[error("set {0} is being written by another import; retry when it finishes")]
    SetBusy(String),
    #[error("set {set_id} already holds {limit} entries, the most a set may have; refusing to add more")]
    TooManyEntries { set_id: String, limit: usize },
    #[error("manifest of set {set_id} is invalid: {reason}")]
    BadManifest { set_id: String, reason: String },
    #[error("blob for {logical_path} in set {set_id} is missing or damaged: {reason}")]
    BlobProblem {
        set_id: String,
        logical_path: String,
        reason: String,
    },
}

/// Helper for `map_err` that attaches the path an I/O error happened on.
pub(crate) fn io_err<P: AsRef<Path>>(path: P) -> impl FnOnce(std::io::Error) -> LibraryError {
    let path = path.as_ref().to_path_buf();
    move |source| LibraryError::Io { path, source }
}
