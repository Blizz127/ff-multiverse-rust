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
//! * **Ordinary registration costs the same at any size.** A bounded set of record files and
//!   one log line, whatever the number of accounts (the version 1 index was rewritten whole, twice, per
//!   registration and stopped accepting registrations for good at about 121,000 accounts). The
//!   number of accounts is bounded by [`StoreOptions::max_accounts`] instead, with a clear
//!   error ([`PersistError::AccountLimitReached`]) that costs nothing and touches no login.
//! * **Account lookups avoid unrelated writes.** Token lookups read in-memory tables. Progress
//!   reads and logins share a progress gate so a scene transaction cannot expose half a result.
//! * **Atomic registration.** [`Store::register`] validates every input before it writes
//!   anything, then creates the account and its avatar together; there is no failure path that
//!   leaves a name taken without a token having been returned. [`Store::release_unclaimed`]
//!   removes a registration whose token never reached its client, and [`Store::mark_claimed`] is
//!   how a server says that one did (the registering session never logs in with the token, so
//!   without it every player who has not logged in again would look like an orphan).
//!   [`Store::register_with_character_and_progress`] includes game-authored initial stats,
//!   money and inventory in that same commit. [`Store::register_with_character_and_initial_state`]
//!   can also seed an opaque story. Only the complete, byte-identical initial file set remains
//!   releasable as an orphan; missing files, earned progress and scene receipts prevent release.
//!   An explicit story seed first raises the index header to version 4, a one-time index rewrite.
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
//! * **A scene can update story and character progress together, once.**
//!   [`Store::commit_scene_progress_with_result`] writes a recoverable intent and durable receipt;
//!   [`Store::get_progress_pair`] reads both records coherently. An uncertain commit refuses
//!   further progress reads and mutations until reopening recovers it. Character saves preserve
//!   receipts automatically. Receipt-bearing character records use schema 2; older servers must
//!   not be used against these records or an outstanding intent.
//! * **Co-op scene continuations commit every participant together.** [`Store::commit_scene_batch`]
//!   persists a bounded owner checkpoint, the frozen roster's reservations, and all exact story
//!   and character after-images through one recoverable intent. Battle results consume one
//!   ordinal alongside rewards, inventory and limit gauges. Participants remain reserved through
//!   scene resumption; finishing releases the guests and retains the owner's pending landing.
//!   [`Store::get_scene_group`] recovers that complete roster from any participant. Ordinary
//!   progress writes and trash rekeying refuse active continuations. The first batch durably
//!   raises the index header to version 4 before its intent: older index-version-2/3 servers
//!   refuse the whole store. Explicit registration story seeds and restored story records also raise this
//!   fence, including legacy story-only archives. Detached archives from index-4 stores carry
//!   manifest version 2 so older import tools also refuse them. Compaction and snapshots retain the fence; rollback
//!   requires a snapshot taken before opt-in. Opening an existing store alone does not upgrade it.
//! * **Explicit field visits commit with their story/resource result.** [`Store::commit_field_visit`]
//!   compares the full [`SceneState`] and retains one exact operation receipt. Enter creates a
//!   new visit identity; Advance increments its revision without forgetting known scratch bits.
//!   Both are idle-only until a separate runtime lifecycle is implemented. Ordinary saves and
//!   scene batches preserve visit metadata. Character envelope 4, intent 3, index header 5 and
//!   detached archive manifest 3 exclude older writers before success; no legacy record is
//!   initialized or upgraded merely by opening it. [`Store::require_compatibility`] lets future
//!   adapter-owned opaque formats explicitly request this fence without inventing a visit.
//! * **The story blob moves forward through one atomic step.** [`Progression`] (one opaque blob per
//!   account and game, read only by the game's adapter) is written with [`Store::put_progression`] or,
//!   when the new value depends on the old one, [`Store::update_progression`]: a read-modify-write under
//!   the writer lock, so two saves for one account that arrive out of order cannot take a story back
//!   (the adapter's merge runs inside the closure). A record that cannot be read is an error and is
//!   never replaced by a default. Without an explicit registration seed or later story write,
//!   no story record exists. The record retains `SCHEMA_VERSION` 1 and the same five keys;
//!   the index-format fence controls compatibility when an opaque story is seeded or imported.
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
//! * **A player's original character is its own record, and it points at progress instead of holding it.**
//!   [`PlayerCharacter`] (`characters/<account>.json`, protocol v8) holds the character's identity (name,
//!   colour, body choice) and, per game, how that game draws it (the **binding**), which canon combat **slot**
//!   it fills (fixed at creation) and **pointers** to where its progress lives ([`ProgressRef`]): the
//!   character-progress record and the story blob stay exactly where the store has always filed them, under
//!   (account, game). It is created together with the account ([`Store::register_with_character`]) or, for an
//!   account that predates it, added at its first login ([`Store::ensure_player_character`]); in neither case
//!   is a file of an older build edited, so a rolled-back server finds every record as it left it.
//! * **A delete is soft, atomic and recoverable.** [`Store::delete_account`] moves everything of one account (its
//!   account record, avatar profile, player-character record and progress) into a timestamped entry of
//!   `<root>/trash/`, frees its name and removes it from every index, as one committed `index.log` entry between a
//!   plan written first and the moves done after it; it never reads or writes another account's file. Opening the
//!   store settles a delete a crash interrupted (finishes it if it was committed, abandons it if not).
//!   [`Store::restore_account`] brings an entry back as a new account (new ids, a new token). The layout and the
//!   crash argument are at the top of `store/trash.rs`.
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
//! `store.rs`, `index.rs` and `store/trash.rs`. State dirs hold token hashes, so files are created owner-only on
//! unix.
//!
//! # Concurrency
//!
//! [`Store`] is `Send + Sync` and every method takes `&self`; share it as `Arc<Store>`. Calls
//! block on disk I/O, so async code should call them from `spawn_blocking`. Writers are
//! serialized with each other. Progress readers share a gate with progress writers so paired
//! scene changes are never observed halfway through; account-only lookups use the index.

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

mod character;
mod error;
mod fsio;
mod index;
mod legacy;
mod lock;
mod model;
mod snapshot;
mod store;

pub use character::{
    CharacterOrigin, GameCharacter, Identity, LegacyView, LinkedProgress, NewPlayerCharacter, PlayerCharacter, ProgressRef,
    MAX_PLAYER_CHARACTER_BYTES,
};
pub use error::{PersistError, Result};
pub use index::StoreCompatibility;
pub use store::field_visit::{FieldVisitV1, FieldVisitEntryV1, FieldVisitOperation, FieldVisitOperationKind, FieldVisitCommitted,
    MAX_FIELD_VISIT_BYTES, MAX_FIELD_VISIT_FIELD_BYTES, MAX_FIELD_VISIT_ENTRY_BYTES, MAX_FIELD_VISIT_ANCHOR_BYTES};
pub use model::{
    game_key, parse_game_key, AccountInfo, AvatarId, AvatarProfile, CharacterProgress, NewCharacterProgress, NewProgression, Progression,
    Registered, INDEX_LOG_VERSION, MAX_APPEARANCE_REF_BYTES, MAX_APPEARANCE_REF_CHARS,
    MAX_ARCHETYPE_BYTES, MAX_BLOB_BYTES, MAX_CHARACTER_RECORD_BYTES, MAX_CHARACTER_PROGRESS_SCHEMA_VERSION, MAX_INDEX_BYTES,
    MAX_RECORD_BYTES, MAX_TOKEN_BYTES, SCHEMA_VERSION,
};
pub use snapshot::{restore_snapshot, SnapshotInfo};
pub use store::restyle::{Restyle, RestyleAdded, RestyleError, RestyleReport};
pub use store::trash::{Deleted, Restored, TrashEntry, TrashState};
pub use store::{Store, StoreOptions, DEFAULT_MAX_ACCOUNTS};
pub use store::location::{SavedLocation, MAX_LOCATION_AREA_BYTES, MAX_LOCATION_RECORD_BYTES, MAX_LOCATION_STORY_BYTES};
pub use store::scene::{ProgressPair, SceneCommit, SceneBatchCommitted, SceneCheckpoint, SceneOperation,
    SceneOperationKind, ScenePhase, SceneReservation, SceneState, MAX_ARRIVAL_METADATA_STEPS, MAX_SCENE_RECEIPTS,
    MAX_SCENE_BATCH_ACCOUNTS, MAX_SCENE_CHECKPOINT_BYTES, MAX_SCENE_REPLAY_BYTES};
