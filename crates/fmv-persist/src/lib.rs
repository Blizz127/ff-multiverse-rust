//! Server-side persistence for FF Multiverse (milestone M2-a): accounts, the persistent
//! custom avatar, and per-game progression, in a file-based store under one state directory.
//!
//! Everything stored here is a reference or an opaque blob, never game asset bytes (plan
//! sections 1, 5, 6). Core never interprets progression blobs; only game adapters do.
//!
//! # Guarantees
//!
//! * **Atomic, durable writes.** Temp file in the same directory, fsync, rename, fsync the
//!   directory. A crash leaves the old file or the new file, never a torn one.
//! * **Single writer.** [`Store::open`] holds `<root>/LOCK` (the owner's pid) for its lifetime.
//!   A second server on the same directory is refused; a lock left by a dead process is taken
//!   over with a warning.
//! * **Versioned records.** Every file carries `schema_version`. A newer version than
//!   [`SCHEMA_VERSION`] is rejected with a clear error instead of being misread.
//! * **Bounded and panic-free.** File sizes, blob sizes and names are capped; corrupt or hostile
//!   files produce errors, never panics.
//! * **No hidden clocks.** Callers pass unix-second timestamps in.
//! * **Tokens.** Account tokens come from the OS CSPRNG, are shown once, and only
//!   `blake3(token)` is stored.
//!
//! The on-disk layout and the crash-consistency argument are documented at the top of
//! `store.rs`. State dirs hold token hashes, so files are created owner-only on unix.
//!
//! # Concurrency
//!
//! [`Store`] is `Send + Sync` and every method takes `&self`; share it as `Arc<Store>`. Calls
//! block on disk I/O, so async code should call them from `spawn_blocking`.

#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

mod error;
mod fsio;
mod lock;
mod model;
mod snapshot;
mod store;

pub use error::{PersistError, Result};
pub use model::{
    game_key, parse_game_key, AccountInfo, AvatarId, AvatarProfile, Progression,
    MAX_APPEARANCE_REF_BYTES, MAX_ARCHETYPE_BYTES, MAX_BLOB_BYTES, MAX_INDEX_BYTES,
    MAX_RECORD_BYTES, MAX_TOKEN_BYTES, SCHEMA_VERSION,
};
pub use snapshot::{restore_snapshot, SnapshotInfo};
pub use store::Store;
