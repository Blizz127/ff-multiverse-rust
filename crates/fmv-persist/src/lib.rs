//! Server-side persistence for FF Multiverse (milestone M2-a): accounts, the persistent
//! custom avatar, and per-game progression, in a file-based store under one state directory.
//!
//! Everything stored here is a reference or an opaque blob, never game asset bytes (plan
//! sections 1, 5, 6). Core never interprets progression blobs; only game adapters do.
//!
//! # Guarantees
//!
//! * **Atomic, durable writes.** Record files: temp file in the same directory, fsync, rename,
//!   fsync the directory. A crash leaves the old file or the new file, never a torn one. The
//!   account index is an append-only, checksummed log: one synced line commits a registration.
//! * **A registration costs the same at any size.** Two small record files and one log line,
//!   whatever the number of accounts (the version 1 index was rewritten whole, twice, per
//!   registration and stopped accepting registrations for good at about 121,000 accounts). The
//!   number of accounts is bounded by [`StoreOptions::max_accounts`] instead, with a clear
//!   error ([`PersistError::AccountLimitReached`]) that costs nothing and touches no login.
//! * **Logins never wait for writes.** Token lookups read in-memory tables; no registration,
//!   avatar or progression write, and no fsync, ever blocks one.
//! * **Atomic registration.** [`Store::register`] validates every input before it writes
//!   anything, then creates the account and its avatar together; there is no failure path that
//!   leaves a name taken without a token having been returned. [`Store::release_unclaimed`]
//!   removes a registration whose token never reached its client, and [`Store::mark_claimed`] is
//!   how a server says that one did (the registering session never logs in with the token, so
//!   without it every player who has not logged in again would look like an orphan).
//! * **Names that cannot impersonate.** Display names are unique by `fmv_core::name_key` (case,
//!   canonical form, width, invisible characters and look-alike letters make no difference),
//!   and a new name must come from one writing system.
//! * **Single writer.** [`Store::open`] holds `<root>/LOCK` (the owner's pid) and an `flock` on
//!   it for its lifetime. A second server on the same directory is refused, also one in
//!   another PID namespace or container that cannot see the owner's pid; a lock left by a dead
//!   process is taken over with a warning.
//! * **Character progress is not story progress.** [`CharacterProgress`] (the character an account
//!   plays, its level, experience, AP and gil, and once a game stores one its inventory, per game)
//!   is its own record in its own file (`progression/<account>/<Game>.character.json`), written
//!   atomically like the others, with an atomic read-modify-write
//!   ([`Store::update_character_progress`]) so that two results for one account add up. It is
//!   separate from [`Progression`], the opaque story blob that also selects an account's story
//!   phase: a battle result must never move a player to another instance. A record that was never
//!   stored is absent, not a default (the game's own data supplies the start, so there is nothing
//!   to migrate), and the game's caps and curves stay with its adapter: the store checks only what
//!   is true of every game.
//! * **The story blob moves forward through one atomic step.** [`Progression`] (one opaque blob per
//!   account and game, read only by the game's adapter) is written with [`Store::put_progression`] or,
//!   when the new value depends on the old one, [`Store::update_progression`]: a read-modify-write under
//!   the writer lock, so two saves for one account that arrive out of order cannot take a story back
//!   (the adapter's merge runs inside the closure). A record that cannot be read is an error and is
//!   never replaced by a default. No record is written for an account that never advanced: an absent
//!   record is a new account, and the record keeps the shape it always had (`SCHEMA_VERSION` 1, the
//!   same five keys), so a build from before the story model reads it too (the blob is opaque there).
//! * **An inventory that was never stored is not an empty one.** The inventory is optional inside
//!   the character record: `None` (no `inventory` key on disk, which is what every record written
//!   before inventories existed looks like) means *never stored*, so the server starts from its
//!   game's own starting stock, which the store never supplies; `Some(vec![])` is a stored, empty
//!   inventory and stays empty. The store checks only its shape (at most
//!   [`MAX_INVENTORY_STACKS`](fmv_core::MAX_INVENTORY_STACKS) stacks, none of zero, no item twice;
//!   `Corrupt` on read, `InvalidField` on write). The record stays at [`SCHEMA_VERSION`] 1, so
//!   going back to a build from before the field loses the inventory when that build next
//!   rewrites the record, and fails the login of an account whose record has grown past that
//!   build's 4 KiB limit (see [`MAX_CHARACTER_RECORD_BYTES`]): roll back only from a snapshot
//!   taken before any inventory was stored.
//! * **Versioned records.** Every file carries `schema_version`. A newer version than
//!   [`SCHEMA_VERSION`] (records) or [`INDEX_LOG_VERSION`] (index) is rejected with a clear
//!   error instead of being misread. A version 1 `index.json` is migrated on open, keeping the
//!   old file as `index.json.v1.bak`.
//! * **Bounded and panic-free.** File sizes, blob sizes and names are capped; corrupt or hostile
//!   files produce errors, never panics.
//! * **No hidden clocks.** Callers pass unix-second timestamps in.
//! * **Tokens.** Account tokens come from the OS CSPRNG, are shown once, and only
//!   `blake3(token)` is stored.
//!
//! The on-disk layout and the crash-consistency argument are documented at the top of
//! `store.rs` and `index.rs`. State dirs hold token hashes, so files are created owner-only on
//! unix.
//!
//! # Concurrency
//!
//! [`Store`] is `Send + Sync` and every method takes `&self`; share it as `Arc<Store>`. Calls
//! block on disk I/O, so async code should call them from `spawn_blocking`. Writers are
//! serialized with each other; readers (everything a login needs) are not blocked by them.

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
mod index;
mod legacy;
mod lock;
mod model;
mod snapshot;
mod store;

pub use error::{PersistError, Result};
pub use model::{
    game_key, parse_game_key, AccountInfo, AvatarId, AvatarProfile, CharacterProgress, Progression,
    Registered, INDEX_LOG_VERSION, MAX_APPEARANCE_REF_BYTES, MAX_APPEARANCE_REF_CHARS,
    MAX_ARCHETYPE_BYTES, MAX_BLOB_BYTES, MAX_CHARACTER_RECORD_BYTES, MAX_INDEX_BYTES,
    MAX_RECORD_BYTES, MAX_TOKEN_BYTES, SCHEMA_VERSION,
};
pub use snapshot::{restore_snapshot, SnapshotInfo};
pub use store::{Store, StoreOptions, DEFAULT_MAX_ACCOUNTS};
