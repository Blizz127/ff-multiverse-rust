//! fmv-library: the owned-content library (plan section 7.3).
//!
//! A content-addressed blob store plus one provenance manifest per *set* (one game, platform,
//! edition, region and revision). Different editions never merge: every set has its own id (see
//! [`set_id_for`]) and an import into a set whose provenance is incompatible is refused.
//!
//! Invariants:
//! - originals (installs, disc images, saves) are only ever opened read-only; each file import
//!   verifies size, mtime and identity before and after the copy;
//! - nothing is ever written next to a source: all writes go under the state directory;
//! - blobs and manifests are written to a temp file in the same directory, fsynced and renamed;
//! - a blob that is already stored is reused only after its content hash was checked, and a
//!   damaged one is replaced (see [`Library::verify_entry`] for the full content check);
//! - in-flight copies are marked live by an advisory lock; copies left behind by a killed
//!   import are reclaimed by the next [`Library::open`];
//! - manifests, set ids and logical paths are validated before they are used to build paths;
//! - the manifest caps hold in BOTH directions: the writer refuses to produce a manifest (more
//!   bytes, entries or disc records than the reader accepts) that [`Library::load_manifest`]
//!   would refuse, so a set can never be written into a state it cannot be read back from;
//! - a source file is resolved (symlinks and all) before it is opened, can be confined to the
//!   tree it was found in ([`ImportBatch::add_file_within`]), and is re-checked against what
//!   was opened, so a symbolic link cannot lead an import out of the tree or be swapped in
//!   behind its back;
//! - single-writer is an advisory lock on a `.lock` file per set. Where the filesystem has no
//!   advisory locks (some network and FUSE mounts) an import goes ahead UNLOCKED and says so
//!   ([`ImportBatch::is_locked`]): two concurrent imports into one set are then the operator's
//!   to avoid; blobs are content-addressed and manifests are replaced atomically, so the worst
//!   case is an entry that has to be imported again;
//! - new set and blob-shard directories are fsynced in their parent, and the half-written
//!   manifest temp files of a killed writer are swept when the library is next opened;
//! - [`Library::remove_set`] drops a set (broken or not) without touching blobs or originals;
//! - each manifest says how far its provenance labels can be trusted ([`Identification`]):
//!   `EditionProvenance` itself only carries the labels, so a guess would otherwise look
//!   exactly like a fact;
//! - the library directory carries a `.gitignore` of `*`, so copies of the owner's data are
//!   never committed even when the state directory sits inside a work tree;
//! - no asset bytes are logged or returned in errors.
//!
//! The library holds the owner's data. It never serves it to other players: the protocol only
//! exchanges fingerprints (the blake3 recorded in each [`ManifestEntry`]).

#![deny(clippy::indexing_slicing, clippy::unwrap_used, clippy::expect_used)]
#![cfg_attr(
    test,
    allow(clippy::indexing_slicing, clippy::unwrap_used, clippy::expect_used)
)]

mod error;
pub mod ff7;
mod fsutil;
mod library;
mod manifest;
mod paths;

pub use error::LibraryError;
pub use fsutil::{
    atomic_write, atomic_write_with, create_private_dir_all, create_private_dir_durable,
    lock_names_path, open_lock_file, pretend_locks_are_unsupported, sweep_stale_temps,
    try_lock_exclusive, LockOutcome, STALE_AFTER,
};
pub use library::{
    hash_file, DroppedDisc, EntryStatus, ImportBatch, ImportOutcome, Library, RemovedSet,
    SetHandle, SourceStamp, DEFAULT_MAX_FILE_SIZE, LIBRARY_VERSION,
};
pub use manifest::{
    DiscContent, DiscDigest, Identification, Manifest, ManifestEntry, MANIFEST_FILE,
    MANIFEST_FORMAT_VERSION, MAX_DISC_RECORDS, MAX_DISC_SERIAL, MAX_IDENTIFICATION_CAVEATS,
    MAX_IDENTIFICATION_METHOD, MAX_IDENTIFICATION_TEXT, MAX_MANIFEST_BYTES, MAX_MANIFEST_ENTRIES,
};
pub use paths::{
    describe_provenance, is_blake3_hex, normalize_logical_path, resolve_state_dir,
    resolve_state_dir_from, set_id_for, validate_set_id, MAX_COMPONENT, MAX_LOGICAL_PATH,
    STATE_DIR_ENV,
};
