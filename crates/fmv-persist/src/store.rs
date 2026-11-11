//! The store: accounts, avatar profiles and per-game progression under one state directory.
//!
//! ```text
//! <root>/LOCK                                   single-writer guard (pid)
//! <root>/index.log                              the account index: names, token hashes, avatar
//!                                               links, id high-water marks (see `index.rs`)
//! <root>/index.json                             format marker (and, once, the version 1 index:
//!                                               see `legacy.rs`)
//! <root>/accounts/<account_id>.json             AccountRecord
//! <root>/avatars/<avatar_id>.json               AvatarProfile
//! <root>/characters/<account_id>.json           PlayerCharacter (identity, per-game binding and slot,
//!                                               pointers to the progress records below; see `character.rs`)
//! <root>/progression/<account_id>/<Game>.json   Progression (the story blob, also the phase key)
//! <root>/progression/<account_id>/<Game>.character.json   CharacterProgress (level, exp, ap, gil
//!                                                         and, once one was stored, the
//!                                                         inventory: a missing `inventory` key
//!                                                         means never stored, `[]` means empty)
//! <root>/trash/<stamp>-<account_id>/            a deleted account's records, moved here whole by
//!                                               `delete_account` (made by the first delete; see `store/trash.rs`)
//! ```
//!
//! # Crash consistency
//!
//! `index.log` is the single commit point for everything that allocates an id or must stay
//! unique. Creating an account (with its avatar, player-character and optional initial
//! character/story progress) writes the record files first (nothing references them yet) and appends **one** log entry last: the name, the token
//! and the ids become real together, or not at all. A crash in between leaves unreferenced record
//! files that the next creation overwrites, never a dangling reference or a reused id. Updates to
//! an existing avatar or progression are one atomic file write each. So is the one record that is
//! created later: the player-character record of an account that predates them
//! ([`Store::ensure_player_character`]), which only ever *adds* a file.
//!
//! # Locks
//!
//! Locks are taken in this order when more than one is needed:
//!
//! 1. **writer** (a `Mutex`): held by every operation that changes anything on disk
//!    (registration, avatar and progression writes, release, compaction, snapshots) for its
//!    whole duration. That keeps [`Store::snapshot`] a true point-in-time copy and makes
//!    "check that the name is free, then take it" one step.
//! 2. **progress_gate** (an `RwLock`): progression readers and login bookkeeping take it
//!    shared; progression writes, scene transactions, deletes and restores take it exclusive.
//!    A reader never sees the middle of a scene's two-record update.
//! 3. **log** (a `Mutex`): held only while an entry is appended and synced, and by the first
//!    login of an account, which records that it happened. Short, and never held across
//!    anything but that one write.
//! 4. **index** (an `RwLock`): the in-memory lookup tables. Readers hold it for a hash lookup;
//!    a writer takes it for the few microseconds it needs to insert after the entry is durable.
//!
//! Account-only lookups use the index without taking the writer mutex. Logins and progress
//! reads take the shared progress gate: they wait for a progress update, but not unrelated
//! registration or avatar writes. A failed scene transaction keeps progress inaccessible
//! until reopen settles its durable intent. First login also records a claim under the log
//! mutex; the first login missing a player-character record takes the writer mutex to add it.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::sync::atomic::{AtomicBool, Ordering};

use fmv_core::{name_key, AccountId, AvatarAppearance, GameId};

use crate::character::{NewPlayerCharacter, PlayerCharacter, MAX_PLAYER_CHARACTER_BYTES};
use crate::error::{PersistError, Result};
use crate::fsio;
use crate::index::{self, AccountEntry, Index, LogWriter, Op, TokenHash, INDEX_LOG_FILE};
use crate::lock::LockGuard;
use crate::model::{
    check_new_appearance_ref, game_key, is_safe_key, AccountInfo, AccountRecord, AvatarId,
    AvatarProfile, CharacterProgress, NewCharacterProgress, NewProgression, Progression, Registered, RegistrationProgress, MAX_BLOB_BYTES,
    MAX_CHARACTER_RECORD_BYTES, MAX_RECORD_BYTES, MAX_TOKEN_BYTES, SCHEMA_VERSION,
};
use crate::snapshot::{self, SnapshotInfo};

mod player_character;
mod progression_cas;
pub(crate) mod field_visit;
pub(crate) mod scene;
pub mod restyle;
pub mod trash;

pub(crate) const ACCOUNTS_DIR: &str = "accounts";
pub(crate) const AVATARS_DIR: &str = "avatars";
pub(crate) const PROGRESSION_DIR: &str = "progression";
pub(crate) const CHARACTERS_DIR: &str = "characters";
const TOKEN_PREFIX: &str = "fmvt_";

/// The lookup tables are sized for the account limit when the store opens, up to this many
/// accounts (about 250 MB of address space, nearly all of it untouched until used); beyond that
/// they grow as needed.
const MAX_PRESIZED_ACCOUNTS: u64 = 1_000_000;

/// The default for [`StoreOptions::max_accounts`].
///
/// The version 1 index stopped accepting registrations for good at about 121,000 accounts (its
/// 16 MiB cap). This default stays below that and is far above any real population; it is
/// there so that registration spam cannot grow the state directory without bound.
pub const DEFAULT_MAX_ACCOUNTS: u64 = 100_000;

/// How a [`Store`] is opened. Build one with `StoreOptions::default()` and the `with_*`
/// methods; more options can be added without breaking callers.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct StoreOptions {
    /// Most accounts the store holds. A registration that would exceed it fails with
    /// [`PersistError::AccountLimitReached`] before any disk work; everything else, logins
    /// included, is unaffected. Lowering it below the number of existing accounts keeps them
    /// all (only new registrations are refused). Default: [`DEFAULT_MAX_ACCOUNTS`].
    pub max_accounts: u64,
}

impl Default for StoreOptions {
    fn default() -> Self {
        Self {
            max_accounts: DEFAULT_MAX_ACCOUNTS,
        }
    }
}

impl StoreOptions {
    pub fn with_max_accounts(mut self, max_accounts: u64) -> Self {
        self.max_accounts = max_accounts;
        self
    }
}

/// State only writers touch (guarded by the writer lock).
struct Counters {
    next_account_id: u64,
    next_avatar_id: u64,
}

type NewAccountRecords = (AccountId, Option<AvatarProfile>, Option<PlayerCharacter>, Option<CharacterProgress>, Option<Progression>);

/// The bytes that are both written and fingerprinted for an untouched registration seed.
fn registration_progress_bytes(record: &CharacterProgress) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(record).map_err(|e| PersistError::InvalidField {
        field: "initial_progress", reason: format!("cannot serialize: {e}"),
    })?;
    bytes.push(b'\n');
    if bytes.len() as u64 > MAX_CHARACTER_RECORD_BYTES {
        return Err(PersistError::InvalidField { field: "initial_progress", reason: "record exceeds size limit".into() });
    }
    Ok(bytes)
}

/// Serialize once: these exact bytes are both written and bound by the orphan fingerprint.
fn registration_story_bytes(record: &Progression) -> Result<Vec<u8>> {
    if record.blob.len() > MAX_BLOB_BYTES {
        return Err(PersistError::BlobTooLarge { len: record.blob.len(), max: MAX_BLOB_BYTES });
    }
    let mut bytes = serde_json::to_vec_pretty(record).map_err(|e| PersistError::InvalidField {
        field: "initial_story", reason: format!("cannot serialize: {e}"),
    })?;
    bytes.push(b'\n');
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(PersistError::InvalidField { field: "initial_story", reason: "record exceeds size limit".into() });
    }
    Ok(bytes)
}

#[cfg(test)]
thread_local! {
    static FAIL_REGISTRATION_BEFORE_COMMIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static FAIL_REGISTRATION_AFTER_CHARACTER_SEED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static FAIL_SCENE_FORMAT_AFTER_RENAME: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn registration_checkpoint() -> Result<()> {
    if FAIL_REGISTRATION_BEFORE_COMMIT.with(|flag| flag.replace(false)) {
        return Err(PersistError::InvalidField { field: "test_checkpoint", reason: "registration interrupted before commit".into() });
    }
    Ok(())
}

/// Handle to one state directory. Holds the single-writer lock until dropped.
///
/// All methods take `&self` and the type is `Send + Sync`, so it can be shared as
/// `Arc<Store>`. Calls do blocking file I/O (including fsync); async callers should use
/// `spawn_blocking`.
pub struct Store {
    root: PathBuf,
    max_accounts: u64,
    /// Lock 1, see the module documentation.
    writer: Mutex<Counters>,
    /// Lock 3.
    log: Mutex<LogWriter>,
    /// Lock 4.
    index: RwLock<Index>,
    warnings: Vec<String>,
    /// Readers take this shared; progression writers take it exclusive after writer.
    progress_gate: RwLock<()>,
    /// An uncertain scene commit requires reopen/recovery before any further mutation.
    transaction_poisoned: AtomicBool,
    /// Last field: the LOCK file is released only after everything else is gone.
    _lock: LockGuard,
}

impl fmt::Debug for Store {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Store")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

fn token_hash(token: &str) -> TokenHash {
    *blake3::hash(token.as_bytes()).as_bytes()
}

/// `fmvt_` + 64 hex characters (256 bits from the OS CSPRNG).
fn new_token() -> Result<String> {
    let mut raw = [0u8; 32];
    File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut raw))
        .map_err(|e| PersistError::Entropy(format!("cannot read /dev/urandom: {e}")))?;
    if raw.iter().all(|&b| b == 0) {
        return Err(PersistError::Entropy(
            "random source returned all zeros".into(),
        ));
    }
    Ok(format!("{TOKEN_PREFIX}{}", hex::encode(raw)))
}

/// The look of a new account's avatar.
struct NewAvatar {
    tint: [u8; 3],
    base_archetype: String,
    per_game_overrides: BTreeMap<String, String>,
}

impl Store {
    /// Open (creating if needed) the state directory at `root` and take its writer lock, with
    /// the default [`StoreOptions`].
    ///
    /// Fails with [`PersistError::Locked`] if another live process holds the lock. A lock left
    /// by a dead process is taken over; that and other recoverable oddities are reported
    /// through `tracing::warn!` and [`Store::open_warnings`]. A directory with a version 1
    /// `index.json` is migrated to the current index format first (see the `legacy` module:
    /// the old file is kept as `index.json.v1.bak`).
    pub fn open(root: impl AsRef<Path>) -> Result<Store> {
        Self::open_with(root, StoreOptions::default())
    }

    /// [`Store::open`] with explicit options.
    pub fn open_with(root: impl AsRef<Path>, options: StoreOptions) -> Result<Store> {
        let root = root.as_ref().to_path_buf();
        fsio::ensure_dir(&root)?;
        let mut warnings = Vec::new();
        // From here on, every early return drops `lock`, which releases the LOCK file.
        let lock = LockGuard::acquire(&root, &mut warnings)?;

        for sub in [ACCOUNTS_DIR, AVATARS_DIR, PROGRESSION_DIR, CHARACTERS_DIR] {
            fsio::ensure_dir(&root.join(sub))?;
        }

        let swept = sweep_all_temp_files(&root)?;
        if !swept.is_empty() {
            let message = format!(
                "removed {} temp file(s) left by an interrupted write",
                swept.len()
            );
            tracing::warn!("{message}");
            warnings.push(message);
        }

        let mut opened = index::open_index(&root, &mut warnings, || has_any_records(&root))?;
        // Size the lookup tables for the account limit now (see `Index::reserve_for`).
        let presized =
            usize::try_from(options.max_accounts.min(MAX_PRESIZED_ACCOUNTS)).unwrap_or(0);
        opened.index.reserve_for(presized);

        let next_account_id = opened.next_account_id;
        let mut store = Store {
            root,
            max_accounts: options.max_accounts,
            writer: Mutex::new(Counters {
                next_account_id: opened.next_account_id,
                next_avatar_id: opened.next_avatar_id,
            }),
            log: Mutex::new(opened.log),
            index: RwLock::new(opened.index),
            warnings,
            progress_gate: RwLock::new(()),
            transaction_poisoned: AtomicBool::new(false),
            _lock: lock,
        };
        // A progress intent is settled before trash recovery can move any of its records.
        store.recover_scene_progress()?;
        {
            let index = store.index.read().unwrap_or_else(PoisonError::into_inner);
            trash::recover(&store.root, &index, next_account_id, &mut store.warnings)?;
            for message in find_unreferenced_records(&store.root, &index)? {
                tracing::warn!("{message}");
                store.warnings.push(message);
            }
        }
        Ok(store)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Recoverable oddities noticed while opening (stale lock taken over, temp files swept,
    /// unreferenced records left by a crash, an index migrated or repaired, names that collide
    /// under the current name rule). Empty for an ordinary clean open.
    pub fn open_warnings(&self) -> &[String] {
        &self.warnings
    }

    /// How many accounts exist.
    pub fn account_count(&self) -> u64 {
        self.index().len() as u64
    }

    /// The configured account limit ([`StoreOptions::max_accounts`]).
    pub fn max_accounts(&self) -> u64 {
        self.max_accounts
    }

    // ---- paths -----------------------------------------------------------------------

    fn log_path(&self) -> PathBuf {
        self.root.join(INDEX_LOG_FILE)
    }

    fn account_path(&self, id: u64) -> PathBuf {
        self.root.join(ACCOUNTS_DIR).join(format!("{id}.json"))
    }

    fn avatar_path(&self, id: u64) -> PathBuf {
        self.root.join(AVATARS_DIR).join(format!("{id}.json"))
    }

    fn character_record_path(&self, account: u64) -> PathBuf {
        self.root.join(CHARACTERS_DIR).join(format!("{account}.json"))
    }

    fn progression_dir(&self, account: u64) -> PathBuf {
        self.root.join(PROGRESSION_DIR).join(account.to_string())
    }

    fn progression_path(&self, account: u64, game: GameId) -> Result<PathBuf> {
        let key = game_key(game);
        if !is_safe_key(&key) {
            return Err(PersistError::InvalidField {
                field: "game",
                reason: format!("{key:?} cannot be used as a file name"),
            });
        }
        Ok(self.progression_dir(account).join(format!("{key}.json")))
    }

    /// `<Game>.character.json` next to the story blob. A game key is letters, digits and `_`
    /// (see `is_safe_key`), so no key can ever spell the `.character` part: the two records of a
    /// game cannot name the same file.
    fn character_path(&self, account: u64, game: GameId) -> Result<PathBuf> {
        let key = game_key(game);
        if !is_safe_key(&key) {
            return Err(PersistError::InvalidField {
                field: "game",
                reason: format!("{key:?} cannot be used as a file name"),
            });
        }
        Ok(self
            .progression_dir(account)
            .join(format!("{key}.character.json")))
    }

    // ---- shared plumbing ---------------------------------------------------------------

    // Poisoning cannot leave anything half-updated: the in-memory state only changes after
    // the matching disk write succeeded, in a few infallible statements. So a poisoned lock
    // is recovered instead of turning one panic into every later call failing.

    /// Lock 1. Every operation that writes holds it for its whole duration.
    fn writer(&self) -> Result<MutexGuard<'_, Counters>> {
        let guard = self.writer.lock().unwrap_or_else(PoisonError::into_inner);
        self.ensure_transaction_healthy()?;
        Ok(guard)
    }

    fn ensure_transaction_healthy(&self) -> Result<()> {
        if self.transaction_poisoned.load(Ordering::Acquire) {
            return Err(PersistError::TransactionRecoveryRequired);
        }
        Ok(())
    }

    fn progress_read(&self) -> Result<RwLockReadGuard<'_, ()>> {
        let guard = self.progress_gate.read().unwrap_or_else(PoisonError::into_inner);
        self.ensure_transaction_healthy()?;
        Ok(guard)
    }

    fn progress_write(&self) -> RwLockWriteGuard<'_, ()> {
        self.progress_gate.write().unwrap_or_else(PoisonError::into_inner)
    }

    /// Lock 3.
    fn log(&self) -> MutexGuard<'_, LogWriter> {
        self.log.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Lock 4, shared.
    fn index(&self) -> RwLockReadGuard<'_, Index> {
        self.index.read().unwrap_or_else(PoisonError::into_inner)
    }

    /// Lock 4, exclusive: only after the matching log entry is durable.
    fn index_mut(&self) -> RwLockWriteGuard<'_, Index> {
        self.index.write().unwrap_or_else(PoisonError::into_inner)
    }

    fn load_account(&self, id: u64) -> Result<AccountRecord> {
        let path = self.account_path(id);
        let record: AccountRecord = fsio::read_json(&path, MAX_RECORD_BYTES)?.ok_or_else(|| {
            PersistError::corrupt(&path, "referenced by the index but the file is missing")
        })?;
        if record.account_id.0 != id {
            return Err(PersistError::corrupt(
                &path,
                format!("record claims account id {}", record.account_id.0),
            ));
        }
        Ok(record)
    }

    fn load_avatar(&self, id: u64) -> Result<AvatarProfile> {
        let path = self.avatar_path(id);
        let profile: AvatarProfile =
            fsio::read_json(&path, MAX_RECORD_BYTES)?.ok_or_else(|| {
                PersistError::corrupt(&path, "referenced by the index but the file is missing")
            })?;
        if profile.avatar_id.0 != id {
            return Err(PersistError::corrupt(
                &path,
                format!("record claims avatar id {}", profile.avatar_id.0),
            ));
        }
        Ok(profile)
    }

    /// Refuse a registration at the account limit. Reads one counter: no disk, no writer lock.
    fn check_capacity(&self) -> Result<()> {
        if self.index().len() as u64 >= self.max_accounts {
            return Err(PersistError::AccountLimitReached {
                max: self.max_accounts,
            });
        }
        Ok(())
    }

    // ---- accounts ----------------------------------------------------------------------

    /// Register an account **and** its avatar in one atomic step: either both exist, with the
    /// name taken and the token valid, or nothing was changed.
    ///
    /// Every input is validated before anything is written, so no bad input can leave a name
    /// reserved without a token that was handed back:
    ///
    /// * `display_name` must pass `AvatarAppearance::validate_new_name` (visible characters
    ///   from one writing system) and have a free `fmv_core::name_key`; otherwise
    ///   [`PersistError::InvalidName`] / [`PersistError::NameTaken`],
    /// * `base_archetype`: non-empty, at most [`MAX_ARCHETYPE_BYTES`](crate::MAX_ARCHETYPE_BYTES)
    ///   bytes, no control characters,
    /// * `appearance_ref`: the per-game appearance reference of the new avatar, as
    ///   `(game, reference)`. Exactly the server's rule: not empty, at most
    ///   [`MAX_APPEARANCE_REF_CHARS`](crate::MAX_APPEARANCE_REF_CHARS) **characters**, no
    ///   control characters. A value that `AvatarAppearance::clean_appearance_ref` produced
    ///   always passes.
    ///
    /// The account limit is checked first and costs nothing, then the token is minted and the
    /// writer lock taken. The disk work is the two record files and one synced log entry,
    /// whatever the number of accounts. `now` is the caller's unix time in seconds.
    ///
    /// This registers an account without a player-character record (what every build before protocol v8
    /// did); a v8 server calls [`Store::register_with_character`].
    pub fn register(
        &self,
        display_name: &str,
        tint: [u8; 3],
        base_archetype: &str,
        appearance_ref: Option<(GameId, &str)>,
        now: u64,
    ) -> Result<Registered> {
        self.register_with_character(display_name, tint, base_archetype, appearance_ref, None, now)
    }

    /// [`Store::register`], and with `character` the player-character record of the new account too, in
    /// the same atomic step: the record is written with the avatar, before the one log entry that makes
    /// the account real, so there is no state in which a name is taken and its character is missing (a
    /// crash in between leaves an unreferenced file that the next registration overwrites). Its identity
    /// is the registration's name and `tint`, plus `character.body`; its origin is
    /// [`CharacterOrigin::Created`]. The record is checked with every other input, before anything is
    /// written (see [`PlayerCharacter`]).
    pub fn register_with_character(
        &self,
        display_name: &str,
        tint: [u8; 3],
        base_archetype: &str,
        appearance_ref: Option<(GameId, &str)>,
        character: Option<NewPlayerCharacter>,
        now: u64,
    ) -> Result<Registered> {
        self.register_with_character_and_progress(display_name, tint, base_archetype, appearance_ref, character, None, now)
            .map(|(registered, _)| registered)
    }

    /// Register with an optional game-authored initial character-progress record, committed
    /// with the account and identity before returning its token. Every input is validated
    /// before any write; no follow-up read or write can strand a committed name without its
    /// initial progress. The returned progress is exactly the record that was written.
    /// An untouched seed does not prevent releasing an unclaimed, undelivered registration.
    #[allow(clippy::too_many_arguments)]
    pub fn register_with_character_and_progress(
        &self,
        display_name: &str,
        tint: [u8; 3],
        base_archetype: &str,
        appearance_ref: Option<(GameId, &str)>,
        character: Option<NewPlayerCharacter>,
        progress: Option<NewCharacterProgress>,
        now: u64,
    ) -> Result<(Registered, Option<CharacterProgress>)> {
        self.register_with_character_and_initial_state(display_name, tint, base_archetype, appearance_ref, character, progress, None, now)
            .map(|(registered, progress, _)| (registered, progress))
    }

    /// Register identity and optional character/story seeds with one account-log commit.
    /// Both seeds must name the same game. Every input and encoded size is validated before
    /// any file changes; both files are durable before the account becomes visible. Returned
    /// records are the exact after-images, without a fallible postcommit read or write.
    /// Any explicit story seed first raises the recognized index-format fence to version 4;
    /// binaries limited to index versions 2 or 3 refuse before modifying its story.
    #[allow(clippy::too_many_arguments)]
    pub fn register_with_character_and_initial_state(
        &self,
        display_name: &str,
        tint: [u8; 3],
        base_archetype: &str,
        appearance_ref: Option<(GameId, &str)>,
        character: Option<NewPlayerCharacter>,
        progress: Option<NewCharacterProgress>,
        story: Option<NewProgression>,
        now: u64,
    ) -> Result<(Registered, Option<CharacterProgress>, Option<Progression>)> {
        let name = AvatarAppearance::validate_new_name(display_name)
            .map_err(|_| PersistError::InvalidName)?;
        let mut per_game_overrides = BTreeMap::new();
        if let Some((game, reference)) = appearance_ref {
            check_new_appearance_ref(reference)?;
            per_game_overrides.insert(game_key(game), reference.to_owned());
        }
        let avatar = NewAvatar {
            tint,
            base_archetype: base_archetype.to_owned(),
            per_game_overrides,
        };
        // The same field checks `create_avatar` and `put_avatar` apply, run on the real values
        // now so that nothing after this point depends on the caller's data.
        avatar_profile(AccountId(0), AvatarId(0), &name, &avatar, now).validate_fields()?;
        if let Some(new) = &character {
            player_character::new_record(AccountId(0), &name, tint, new, now).validate()?;
        }
        if let Some(new) = &progress {
            let largest = new.record(AccountId(u64::MAX), now);
            largest.validate()?;
            registration_progress_bytes(&largest)?;
            self.character_path(0, new.game)?;
        }
        if let Some(new) = &story {
            registration_story_bytes(&new.record(AccountId(u64::MAX), now))?;
            self.progression_path(0, new.game)?;
        }
        if progress.as_ref().zip(story.as_ref()).is_some_and(|(progress, story)| progress.game != story.game) {
            return Err(PersistError::InvalidField { field: "initial_state", reason: "character and story seeds must belong to the same game".into() });
        }

        self.check_capacity()?;
        let token = new_token()?;
        let (account_id, profile, record, progress, story) =
            self.insert_account(name, token_hash(&token), now, Some(avatar), character, progress, story)?;
        let avatar = profile.ok_or_else(|| {
            PersistError::corrupt(&self.log_path(), "a registration produced no avatar")
        })?;
        Ok((Registered {
            account_id,
            token,
            avatar,
            character: record,
        }, progress, story))
    }

    /// Create an account without an avatar. Returns its id and the plaintext token, which is
    /// shown exactly once; only `blake3(token)` is stored. Prefer [`Store::register`], which
    /// creates the avatar in the same step.
    ///
    /// `display_name` must pass `AvatarAppearance::validate_new_name` and have a free
    /// `fmv_core::name_key` (case, width, look-alike characters and invisible characters make
    /// no difference). `now` is the caller's unix time in seconds.
    pub fn create_account(&self, display_name: &str, now: u64) -> Result<(AccountId, String)> {
        let name = AvatarAppearance::validate_new_name(display_name)
            .map_err(|_| PersistError::InvalidName)?;
        self.check_capacity()?;
        let token = new_token()?;
        let (account, _, _, _, _) = self.insert_account(name, token_hash(&token), now, None, None, None, None)?;
        Ok((account, token))
    }

    /// The write path shared by `register` and `create_account`: take the writer lock, check
    /// that the name is free, write the record files, append the log entry.
    #[allow(clippy::too_many_arguments)]
    fn insert_account(
        &self,
        name: String,
        hash: TokenHash,
        now: u64,
        avatar: Option<NewAvatar>,
        character: Option<NewPlayerCharacter>,
        progress: Option<NewCharacterProgress>,
        story: Option<NewProgression>,
    ) -> Result<NewAccountRecords> {
        let key = name_key(&name);
        let mut counters = self.writer()?;
        {
            let index = self.index();
            if index.len() as u64 >= self.max_accounts {
                return Err(PersistError::AccountLimitReached {
                    max: self.max_accounts,
                });
            }
            if index.name_owner(&key).is_some() {
                return Err(PersistError::NameTaken(name));
            }
            if index.token_owner(&hash).is_some() {
                // 256 random bits colliding means the entropy source is broken. Never overwrite
                // another account's token.
                return Err(PersistError::Entropy("token collision".into()));
            }
        }
        let id = counters.next_account_id;
        let next_account = id.checked_add(1).ok_or(PersistError::IdExhausted)?;
        let avatar_id = counters.next_avatar_id;
        let next_avatar = avatar_id.checked_add(1).ok_or(PersistError::IdExhausted)?;

        let progress_dir = self.progression_dir(id);
        let initial = progress.map(|new| new.record(AccountId(id), now));
        let initial_bytes = initial.as_ref().map(registration_progress_bytes).transpose()?;
        let initial_story = story.map(|new| new.record(AccountId(id), now));
        let story_bytes = initial_story.as_ref().map(registration_story_bytes).transpose()?;
        let registration_progress = initial.as_ref().zip(initial_bytes.as_ref()).map(|(record, bytes)| RegistrationProgress {
            game: record.game, digest: *blake3::hash(bytes).as_bytes(),
        });
        let registration_story = initial_story.as_ref().zip(story_bytes.as_ref()).map(|(record, bytes)| RegistrationProgress {
            game: record.game, digest: *blake3::hash(bytes).as_bytes(),
        });
        let _progress_gate = initial_story.as_ref().map(|_| self.progress_write());
        if initial_story.is_some() { self.ensure_scene_batch_format(&counters)?; }
        self.clear_uncommitted_account_state(id)?;

        // 1. The records. Nothing references them yet.
        let record = AccountRecord {
            schema_version: SCHEMA_VERSION,
            account_id: AccountId(id),
            display_name: name.clone(),
            token_hash: hex::encode(hash),
            created_at: now,
            registration_progress,
            registration_story,
        };
        fsio::write_json(&self.account_path(id), &record, MAX_RECORD_BYTES)?;
        let profile = match &avatar {
            Some(new) => {
                let profile = avatar_profile(AccountId(id), AvatarId(avatar_id), &name, new, now);
                fsio::write_json(&self.avatar_path(avatar_id), &profile, MAX_RECORD_BYTES)?;
                Some(profile)
            }
            None => None,
        };
        // The player-character record goes down with the avatar: nothing references it before the log
        // entry below, and a crash before it leaves a file the next creation with this id overwrites.
        let character_record = match (&avatar, &character) {
            (Some(new), Some(wanted)) => {
                let record = player_character::new_record(AccountId(id), &name, new.tint, wanted, now);
                fsio::write_json(&self.character_record_path(id), &record, MAX_PLAYER_CHARACTER_BYTES)?;
                Some(record)
            }
            (None, Some(_)) => {
                return Err(PersistError::InvalidField {
                    field: "character",
                    reason: "a player-character record needs the avatar it is created with".into(),
                })
            }
            (_, None) => None,
        };
        if let (Some(initial), Some(bytes)) = (&initial, &initial_bytes) {
            fsio::ensure_dir(&progress_dir)?;
            fsio::atomic_write(&self.character_path(id, initial.game)?, bytes)?;
        }
        #[cfg(test)]
        if FAIL_REGISTRATION_AFTER_CHARACTER_SEED.with(|flag| flag.replace(false)) {
            return Err(PersistError::InvalidField { field: "test_checkpoint", reason: "registration interrupted between initial files".into() });
        }
        if let (Some(initial), Some(bytes)) = (&initial_story, &story_bytes) {
            fsio::ensure_dir(&progress_dir)?;
            fsio::atomic_write(&self.progression_path(id, initial.game)?, bytes)?;
        }
        #[cfg(test)]
        registration_checkpoint()?;

        // 2. The commit point: one synced log entry. Memory changes only after it is durable.
        let linked_avatar = profile.as_ref().map(|_| avatar_id);
        let entry = AccountEntry {
            name,
            token: hash,
            avatar: linked_avatar,
            created_at: now,
            claimed: false,
        };
        let op = Op::Account {
            id,
            name: entry.name.clone(),
            token: hex::encode(hash),
            at: now,
            avatar: linked_avatar,
            claimed: false,
        };
        {
            let mut log = self.log();
            log.append(&op)?;
            self.index_mut().add_account(id, entry, key);
        }
        counters.next_account_id = next_account;
        if linked_avatar.is_some() {
            counters.next_avatar_id = next_avatar;
        }
        Ok((AccountId(id), profile, character_record, initial, initial_story))
    }

    /// Remove state left before an account-allocation commit. Both registration and restore
    /// may allocate the next uncommitted id; neither may inherit another attempt's progress
    /// or player character. Caller holds writer (and has not committed this id).
    fn clear_uncommitted_account_state(&self, id: u64) -> Result<()> {
        self.log().ensure_healthy()?;
        if self.index().contains_account(id) {
            return Err(PersistError::corrupt(&self.account_path(id), "cannot clear a committed account"));
        }
        let progress_dir = self.progression_dir(id);
        match std::fs::remove_dir_all(&progress_dir) {
            Ok(()) => fsio::sync_dir(&self.root.join(PROGRESSION_DIR))
                .map_err(|e| PersistError::io("sync orphan progress removal", &progress_dir, e))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
            Err(e) => return Err(PersistError::io("remove orphan account progress", &progress_dir, e)),
        }
        let character_path = self.character_record_path(id);
        match std::fs::remove_file(&character_path) {
            Ok(()) => fsio::sync_dir(&self.root.join(CHARACTERS_DIR))
                .map_err(|e| PersistError::io("sync orphan character removal", &character_path, e))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
            Err(e) => return Err(PersistError::io("remove orphan account character", &character_path, e)),
        }
        Ok(())
    }

    /// Resolve a token to its account. Unknown, malformed and oversized tokens all fail with
    /// [`PersistError::InvalidToken`].
    ///
    /// Reads the in-memory index under the shared progress gate, so it waits for a scene
    /// commit but not unrelated writes. The first successful login also records that its token
    /// has been used (which is what keeps `release_unclaimed` away from it); that is one
    /// appended log entry, and a failure to write it never fails the login.
    pub fn authenticate(&self, token: &str) -> Result<AccountId> {
        let _read = self.progress_read()?;
        if token.len() > MAX_TOKEN_BYTES {
            return Err(PersistError::InvalidToken);
        }
        let hash = token_hash(token);
        let (id, claimed) = {
            let index = self.index();
            let id = index.token_owner(&hash).ok_or(PersistError::InvalidToken)?;
            let claimed = index.account(id).is_none_or(|entry| entry.claimed);
            (id, claimed)
        };
        if !claimed {
            self.claim(id)?;
        }
        Ok(AccountId(id))
    }

    /// Record that the account's token has been used. Decided under the log lock, which is
    /// also where `release_unclaimed` decides: either this is recorded first (and the release
    /// refuses) or the release is (and this login fails like any unknown token).
    fn claim(&self, id: u64) -> Result<()> {
        let mut log = self.log();
        self.ensure_transaction_healthy()?;
        match self.index().account(id) {
            None => return Err(PersistError::InvalidToken),
            Some(entry) if entry.claimed => return Ok(()),
            Some(_) => {}
        }
        if let Err(e) = log.append(&Op::Claim { account: id }) {
            // The login is valid, so it must not fail on bookkeeping. Remember it for this
            // run; the next run asks again.
            tracing::warn!(account = id, error = %e, "could not record a first login in the index log");
        }
        self.index_mut().set_claimed(id);
        Ok(())
    }

    /// Record that the owner of `account` holds its token, without using the token to log in.
    ///
    /// A registration is answered with the token itself, so the registering session never logs in
    /// with it and the account would stay "unclaimed" (an orphan candidate for
    /// [`Store::release_unclaimed`] and the `fmv-accounts` tool) even though its owner plays on
    /// with the token they were just given. A server therefore calls this when the client answers
    /// the reply that carried a new token (its first message after it): from then on the token
    /// has been delivered as far as the server can tell, and only an account whose token never got
    /// that far counts as an orphan. (A client that pipelines a message right behind its first
    /// one, without reading the reply, is claimed too; that costs nothing beyond what open
    /// registration already allows.) It has the same effect, and writes the same log entry, as the
    /// first login with the token ([`Store::authenticate`]).
    ///
    /// `Ok(true)` if this call recorded it, `Ok(false)` if the account was already claimed,
    /// [`PersistError::UnknownAccount`] if there is no such account (for example it was released
    /// in the meantime: its token is void). Decided under the log lock, like a login: either this
    /// is recorded first (and a release refuses) or the release is (and this fails). A failure to
    /// write the entry is logged, not returned: the account counts as claimed for the rest of this
    /// run and the next run asks again, as for a login.
    pub fn mark_claimed(&self, account: AccountId) -> Result<bool> {
        let _read = self.progress_read()?;
        let mut log = self.log();
        self.ensure_transaction_healthy()?;
        match self.index().account(account.0) {
            None => return Err(PersistError::UnknownAccount(account)),
            Some(entry) if entry.claimed => return Ok(false),
            Some(_) => {}
        }
        if let Err(e) = log.append(&Op::Claim { account: account.0 }) {
            tracing::warn!(account = account.0, error = %e, "could not record in the index log that an account's owner holds its token");
        }
        self.index_mut().set_claimed(account.0);
        Ok(true)
    }

    /// `Ok(None)` for an unknown account.
    pub fn account_info(&self, id: AccountId) -> Result<Option<AccountInfo>> {
        if !self.index().contains_account(id.0) {
            return Ok(None);
        }
        let record = self.load_account(id.0)?;
        Ok(Some(AccountInfo {
            account_id: id,
            display_name: record.display_name,
            created_at: record.created_at,
        }))
    }

    /// Look an account up by display name. The name is compared by `fmv_core::name_key`, so
    /// case, canonical form, width, invisible characters and look-alike letters make no
    /// difference.
    pub fn find_account_by_name(&self, display_name: &str) -> Option<AccountId> {
        let key = name_key(display_name);
        if key.is_empty() {
            return None;
        }
        self.index().name_owner(&key).map(AccountId)
    }

    // ---- orphaned registrations ----------------------------------------------------------

    /// Accounts whose token has never been used to log in and whose owner nobody has recorded as
    /// holding it ([`Store::mark_claimed`]), with their `created_at`, oldest id first. These are
    /// the candidates for [`Store::release_unclaimed`]: a registration whose token never reached
    /// its client (the connection dropped before the reply), or, for a caller that never calls
    /// `mark_claimed`, one whose client never logged in with the token. Accounts migrated from a
    /// version 1 index are never listed.
    pub fn unclaimed_accounts(&self) -> Vec<(AccountId, u64)> {
        let mut found: Vec<(AccountId, u64)> = self
            .index()
            .iter()
            .filter(|(_, entry)| !entry.claimed)
            .map(|(id, entry)| (AccountId(id), entry.created_at))
            .collect();
        found.sort_unstable();
        found
    }

    /// Remove an orphaned registration so that its name can be registered again.
    ///
    /// Allowed only for an account whose token has **never** been used to log in and whose owner
    /// is not recorded as holding it ([`Store::mark_claimed`]; see
    /// [`Store::unclaimed_accounts`]) and that has no stored progression beyond the exact,
    /// untouched character/story seed files written during registration; otherwise it fails
    /// with [`PersistError::NotReleasable`] and nothing changes. The check and the release are
    /// decided under the same lock a first login takes, so an account whose token was just
    /// accepted cannot be released. The account id is retired for good (ids are never reused),
    /// the name becomes free, and the account's token stops working.
    ///
    /// Intended for an operator or a janitor that releases accounts older than some grace
    /// period (a client that registered but lost its connection normally retries within
    /// seconds, so a day is generous). Releasing is one synced log entry; removing the record
    /// files afterwards is best effort and any leftover is only reported on the next open.
    pub fn release_unclaimed(&self, account: AccountId) -> Result<()> {
        let _counters = self.writer()?;
        let _progress = self.progress_write();
        let avatar = self
            .index()
            .account(account.0)
            .ok_or(PersistError::UnknownAccount(account))?
            .avatar;
        let progression_dir = self.progression_dir(account.0);
        let pristine_seed = self.pristine_registration_progress(account)?.ok_or(PersistError::NotReleasable {
            account, reason: "its progression files differ from the complete registration seed",
        })?;
        {
            let mut log = self.log();
            let claimed = self
                .index()
                .account(account.0)
                .is_none_or(|entry| entry.claimed);
            if claimed {
                return Err(PersistError::NotReleasable {
                    account,
                    reason:
                        "its token has been used to log in, or its owner is recorded as holding it",
                });
            }
            log.append(&Op::Release { account: account.0 })?;
            self.index_mut().remove_account(account.0);
        }

        // The account is gone; only tidying is left.
        let mut leftovers = vec![self.account_path(account.0), self.character_record_path(account.0)];
        leftovers.extend(avatar.map(|id| self.avatar_path(id)));
        leftovers.extend(pristine_seed);
        for path in leftovers {
            if let Err(e) = std::fs::remove_file(&path) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(path = %path.display(), error = %e, "could not remove a released account's record");
                }
            }
        }
        if let Err(e) = std::fs::remove_dir(&progression_dir) {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(path = %progression_dir.display(), error = %e, "could not remove a released account's progression directory");
            }
        }
        Ok(())
    }

    /// Only the complete byte-identical initial file set may be discarded before token
    /// delivery. A missing file (including an empty directory), extra entry, receipt, reward
    /// or any other edit blocks release. No seed metadata and no files is a pristine old
    /// registration. Directory/symlink substitutions are never treated as absent seeds.
    fn pristine_registration_progress(&self, account: AccountId) -> Result<Option<Vec<PathBuf>>> {
        let path = self.account_path(account.0);
        let Some(record) = fsio::read_json::<AccountRecord>(&path, MAX_RECORD_BYTES)? else { return Ok(None) };
        if record.account_id != account { return Err(PersistError::corrupt(&path, "registration metadata belongs to another account")); }
        if record.registration_progress.as_ref().zip(record.registration_story.as_ref()).is_some_and(|(a, b)| a.game != b.game) {
            return Err(PersistError::corrupt(&path, "registration seeds belong to different games"));
        }
        let mut expected = BTreeMap::new();
        if let Some(seed) = record.registration_progress {
            expected.insert(format!("{}.character.json", game_key(seed.game)),
                (self.character_path(account.0, seed.game)?, seed.digest, MAX_CHARACTER_RECORD_BYTES));
        }
        if let Some(seed) = record.registration_story {
            expected.insert(format!("{}.json", game_key(seed.game)),
                (self.progression_path(account.0, seed.game)?, seed.digest, MAX_RECORD_BYTES));
        }
        let dir = self.progression_dir(account.0);
        match std::fs::symlink_metadata(&dir) {
            Ok(metadata) if !metadata.is_dir() => return Ok(None),
            Ok(_) => {},
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
            Err(e) => return Err(PersistError::io("inspect registration seed directory", &dir, e)),
        }
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => Some(entries),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(PersistError::io("list registration seed files", &dir, e)),
        };
        let mut found = Vec::new();
        if let Some(entries) = entries {
            for entry in entries {
                let entry = entry.map_err(|e| PersistError::io("read registration seed entry", &dir, e))?;
                let kind = entry.file_type().map_err(|e| PersistError::io("inspect registration seed entry", &entry.path(), e))?;
                if !kind.is_file() { return Ok(None); }
                let Ok(name) = entry.file_name().into_string() else { return Ok(None) };
                if !expected.contains_key(&name) { return Ok(None); }
                found.push(name);
            }
        }
        if found.len() != expected.len() { return Ok(None); }
        let mut paths = Vec::with_capacity(expected.len());
        for (path, digest, max) in expected.into_values() {
            let Some(bytes) = fsio::read_capped(&path, max)? else { return Ok(None) };
            if *blake3::hash(&bytes).as_bytes() != digest { return Ok(None); }
            paths.push(path);
        }
        Ok(Some(paths))
    }

    /// [`Store::release_unclaimed`] for a caller that releases on a best-effort basis, for
    /// example a server that could not deliver the reply of a registration: `Ok(true)` when the
    /// account was released, `Ok(false)` when there is nothing to release (the account is
    /// unknown, its token has been used, or it has progress beyond its untouched registration seed), `Err` only when the
    /// store itself failed.
    pub fn release_if_unclaimed(&self, account: AccountId) -> Result<bool> {
        match self.release_unclaimed(account) {
            Ok(()) => Ok(true),
            Err(PersistError::NotReleasable { .. } | PersistError::UnknownAccount(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    // ---- avatars -----------------------------------------------------------------------

    /// Create the account's avatar. Its display name is the account's. One avatar per account.
    /// (`register` does this together with the account; this is for an account that was made
    /// without one.)
    pub fn create_avatar(
        &self,
        account: AccountId,
        tint: [u8; 3],
        base_archetype: &str,
        now: u64,
    ) -> Result<AvatarProfile> {
        let mut counters = self.writer()?;
        match self.index().account(account.0) {
            None => return Err(PersistError::UnknownAccount(account)),
            Some(entry) if entry.avatar.is_some() => {
                return Err(PersistError::AvatarExists(account))
            }
            Some(_) => {}
        }
        let display_name = self.load_account(account.0)?.display_name;
        let id = counters.next_avatar_id;
        let next_id = id.checked_add(1).ok_or(PersistError::IdExhausted)?;

        let new = NewAvatar {
            tint,
            base_archetype: base_archetype.to_owned(),
            per_game_overrides: BTreeMap::new(),
        };
        let profile = avatar_profile(account, AvatarId(id), &display_name, &new, now);
        profile.validate_fields()?;
        // A possibly committed registration may already own this avatar id on disk.
        self.log().ensure_healthy()?;
        // 1. The record. 2. The commit point.
        fsio::write_json(&self.avatar_path(id), &profile, MAX_RECORD_BYTES)?;
        {
            let mut log = self.log();
            log.append(&Op::Avatar {
                account: account.0,
                avatar: id,
            })?;
            self.index_mut().link_avatar(account.0, id);
        }
        counters.next_avatar_id = next_id;
        Ok(profile)
    }

    /// Replace an existing avatar with `profile`, which must carry the avatar id and account id
    /// it was loaded with. `updated_at` is the caller's. `display_name` must still match the
    /// account's name (same `fmv_core::name_key`, so a change of case is fine); renames are not
    /// supported yet.
    ///
    /// Appearance references are checked like a new one (at most
    /// [`MAX_APPEARANCE_REF_CHARS`](crate::MAX_APPEARANCE_REF_CHARS) characters), except that a
    /// reference an older build stored under its byte limit
    /// ([`MAX_APPEARANCE_REF_BYTES`](crate::MAX_APPEARANCE_REF_BYTES)) is still accepted, so
    /// such an avatar can be updated.
    pub fn put_avatar(&self, profile: &AvatarProfile) -> Result<()> {
        profile.validate_fields()?;
        let name = AvatarAppearance::sanitize_name(&profile.display_name)
            .ok_or(PersistError::InvalidName)?;

        // Held across the write so snapshots and other writers see whole operations only.
        let _counters = self.writer()?;
        {
            let index = self.index();
            let entry = index
                .account(profile.account_id.0)
                .filter(|entry| entry.avatar == Some(profile.avatar_id.0))
                .ok_or(PersistError::UnknownAvatar(profile.avatar_id))?;
            // The account's own key, not the name table: an account that lost a name collision
            // at migration still owns its avatar.
            if name_key(&name) != name_key(&entry.name) {
                return Err(PersistError::InvalidField {
                    field: "display_name",
                    reason: "must match the account's display name (up to case); renaming is \
                             not supported yet"
                        .into(),
                });
            }
        }
        let mut stored = profile.clone();
        stored.schema_version = SCHEMA_VERSION;
        stored.display_name = name;
        fsio::write_json(
            &self.avatar_path(profile.avatar_id.0),
            &stored,
            MAX_RECORD_BYTES,
        )
    }

    /// `Ok(None)` for an unknown avatar id.
    pub fn get_avatar(&self, id: AvatarId) -> Result<Option<AvatarProfile>> {
        if !self.index().contains_avatar(id.0) {
            return Ok(None);
        }
        self.load_avatar(id.0).map(Some)
    }

    /// The account's avatar, if it has one.
    pub fn avatar_for_account(&self, account: AccountId) -> Result<Option<AvatarProfile>> {
        let linked = self
            .index()
            .account(account.0)
            .and_then(|entry| entry.avatar);
        let Some(id) = linked else {
            return Ok(None);
        };
        let profile = self.load_avatar(id)?;
        if profile.account_id != account {
            return Err(PersistError::corrupt(
                &self.avatar_path(id),
                format!("record claims account {:?}", profile.account_id),
            ));
        }
        Ok(Some(profile))
    }

    // ---- progression -------------------------------------------------------------------

    /// The account's progression for `game`, or an empty default if none was stored yet.
    /// Fails with [`PersistError::UnknownAccount`] for an account that does not exist.
    pub fn get_progression(&self, account: AccountId, game: GameId) -> Result<Progression> {
        let _read = self.progress_read()?;
        if !self.index().contains_account(account.0) {
            return Err(PersistError::UnknownAccount(account));
        }
        self.read_progression(account, game)
    }

    fn read_progression(&self, account: AccountId, game: GameId) -> Result<Progression> {
        let path = self.progression_path(account.0, game)?;
        let Some(progression) = fsio::read_json::<Progression>(&path, MAX_RECORD_BYTES)? else {
            return Ok(Progression::empty(account, game));
        };
        if progression.account_id != account || progression.game != game {
            return Err(PersistError::corrupt(
                &path,
                format!(
                    "record is for account {:?} / {}, not the requested account {:?} / {}",
                    progression.account_id,
                    game_key(progression.game),
                    account,
                    game_key(game)
                ),
            ));
        }
        Ok(progression)
    }

    /// Store (replacing) the account's progression for `progression.game`. Other games are
    /// untouched. The blob is capped at [`MAX_BLOB_BYTES`]. An active durable scene
    /// continuation or battle reservation refuses ordinary progress writes.
    pub fn put_progression(&self, progression: &Progression) -> Result<()> {
        if progression.blob.len() > MAX_BLOB_BYTES {
            return Err(PersistError::BlobTooLarge {
                len: progression.blob.len(),
                max: MAX_BLOB_BYTES,
            });
        }
        let path = self.progression_path(progression.account_id.0, progression.game)?;

        let _counters = self.writer()?;
        let _progress = self.progress_write();
        if !self.index().contains_account(progression.account_id.0) {
            return Err(PersistError::UnknownAccount(progression.account_id));
        }
        self.ensure_no_scene_continuation(progression.account_id, progression.game)?;
        fsio::ensure_dir(&self.progression_dir(progression.account_id.0))?;
        let mut stored = progression.clone();
        stored.schema_version = SCHEMA_VERSION;
        fsio::write_json(&path, &stored, MAX_RECORD_BYTES)
    }

    /// Read-modify-write of the account's [`Progression`] for `game`, as one step that no other
    /// writer can interleave with: `change` gets the stored record (the empty default if there is none
    /// yet, as [`Store::get_progression`] returns it) and returns the record to store, which is
    /// returned in turn. This is what lets a story that only moves forward stay so when two writes for
    /// one account arrive out of order (the adapter's merge decides, inside `change`).
    ///
    /// `change` runs while the store's writer lock is held: it must be quick and must not call back
    /// into this store (the lock is not reentrant, so that would deadlock). What it returns must be
    /// for the same account and game and its blob at most [`MAX_BLOB_BYTES`]; otherwise nothing is
    /// written and the old record stays. A stored record that cannot be read ([`PersistError::Corrupt`],
    /// [`PersistError::FutureSchema`], [`PersistError::TooLarge`]) is an error and is never replaced by
    /// a default. Only the `blob` is the caller's to set: `schema_version` is stamped and the
    /// `updated_at` it returns is stored as given, so pass the time of the write.
    pub fn update_progression<F>(&self, account: AccountId, game: GameId, change: F) -> Result<Progression>
    where
        F: FnOnce(Progression) -> Progression,
    {
        let path = self.progression_path(account.0, game)?;

        let _counters = self.writer()?;
        let _progress = self.progress_write();
        if !self.index().contains_account(account.0) {
            return Err(PersistError::UnknownAccount(account));
        }
        self.ensure_no_scene_continuation(account, game)?;
        let mut next = change(self.read_progression(account, game)?);
        if next.account_id != account || next.game != game {
            return Err(PersistError::InvalidField {
                field: "account_id",
                reason: format!(
                    "the updated record is for account {:?} / {}, not the account {:?} / {} it replaces",
                    next.account_id,
                    game_key(next.game),
                    account,
                    game_key(game)
                ),
            });
        }
        if next.blob.len() > MAX_BLOB_BYTES {
            return Err(PersistError::BlobTooLarge { len: next.blob.len(), max: MAX_BLOB_BYTES });
        }
        next.schema_version = SCHEMA_VERSION;
        fsio::ensure_dir(&self.progression_dir(account.0))?;
        fsio::write_json(&path, &next, MAX_RECORD_BYTES)?;
        Ok(next)
    }

    // ---- character progress ------------------------------------------------------------

    /// The account's [`CharacterProgress`] for `game`, or `None` if none was ever stored (the
    /// caller starts from its game's own starting values; nothing is migrated). It is a separate
    /// record from [`Store::get_progression`]'s story blob. Fails with
    /// [`PersistError::UnknownAccount`] for an account that does not exist, and with
    /// [`PersistError::Corrupt`] / [`PersistError::FutureSchema`] / [`PersistError::TooLarge`]
    /// for a file that is damaged, written by a newer build or oversized: such a file is never
    /// replaced by a default behind the caller's back. A damaged **inventory** (a list over
    /// [`MAX_INVENTORY_STACKS`](fmv_core::MAX_INVENTORY_STACKS) stacks, a stack of zero, an item
    /// twice, or numbers and types that are not an inventory) makes the whole record `Corrupt`.
    ///
    /// A record found without an inventory is not an empty one: its
    /// [`inventory`](CharacterProgress::inventory) is `None` ("never stored", so the caller starts
    /// from its game's starting stock), which is what every record written before inventories
    /// existed looks like. `Some(vec![])` is a stored, empty inventory.
    ///
    /// Takes the shared progress gate, so it cannot observe a partially committed scene.
    /// Use `get_progress_pair` when both character and story must belong to the same snapshot.
    pub fn get_character_progress(
        &self,
        account: AccountId,
        game: GameId,
    ) -> Result<Option<CharacterProgress>> {
        let _read = self.progress_read()?;
        if !self.index().contains_account(account.0) {
            return Err(PersistError::UnknownAccount(account));
        }
        self.read_character_progress(account, game)
    }

    /// Return the existing record unchanged, or create the caller's legacy baseline exactly
    /// once under the writer lock. `make` is only called when the file is absent, never for
    /// a malformed/future record; it must not call back into this store. Existing records
    /// (including inventories and scene receipts) are neither rewritten nor normalized.
    pub fn ensure_character_progress<F>(&self, account: AccountId, game: GameId, make: F) -> Result<CharacterProgress>
    where F: FnOnce() -> CharacterProgress,
    {
        let path = self.character_path(account.0, game)?;
        let _counters = self.writer()?;
        let _progress = self.progress_write();
        if !self.index().contains_account(account.0) { return Err(PersistError::UnknownAccount(account)); }
        if let Some(record) = self.read_character_progress(account, game)? { return Ok(record); }
        let mut record = make();
        if record.account_id != account || record.game != game {
            return Err(PersistError::InvalidField { field: "account_id", reason: "initial record belongs to another account or game".into() });
        }
        record.validate()?;
        record.schema_version = SCHEMA_VERSION;
        fsio::ensure_dir(&self.progression_dir(account.0))?;
        self.write_character_record(&path, record)
    }

    fn read_character_progress(
        &self,
        account: AccountId,
        game: GameId,
    ) -> Result<Option<CharacterProgress>> {
        let path = self.character_path(account.0, game)?;
        let Some(stored) = scene::CharacterRecord::read(&path)?
        else {
            return Ok(None);
        };
        let record = stored.progress;
        if record.account_id != account || record.game != game {
            return Err(PersistError::corrupt(
                &path,
                format!(
                    "record is for account {:?} / {}, not the requested account {:?} / {}",
                    record.account_id,
                    game_key(record.game),
                    account,
                    game_key(game)
                ),
            ));
        }
        record
            .validate()
            .map_err(|e| PersistError::corrupt(&path, e.to_string()))?;
        Ok(Some(record))
    }

    /// Store (replacing) the account's [`CharacterProgress`] for `record.game`, atomically (a
    /// crash leaves the old record or the new one). Other games and the story blob are
    /// untouched. The record must pass the checks every game shares (`level` at least 1; an
    /// inventory, if it has one, of at most [`MAX_INVENTORY_STACKS`](fmv_core::MAX_INVENTORY_STACKS)
    /// stacks, none of them zero and no item twice, else [`PersistError::InvalidField`] with
    /// `field: "inventory"`; the game's own caps are its adapter's business) and the account must
    /// exist. A refused record leaves the old one in place. Prefer
    /// [`Store::update_character_progress`] when the new value depends on the old one.
    ///
    /// **The whole record is replaced, the inventory included.** A record whose `inventory` is
    /// `None` (anything built with [`CharacterProgress::new`]) clears a stored inventory, back to
    /// "never stored"; to change other fields and keep the items, start from the record that was
    /// read.
    pub fn put_character_progress(&self, record: &CharacterProgress) -> Result<()> {
        record.validate()?;
        let path = self.character_path(record.account_id.0, record.game)?;

        let _counters = self.writer()?;
        let _progress = self.progress_write();
        if !self.index().contains_account(record.account_id.0) {
            return Err(PersistError::UnknownAccount(record.account_id));
        }
        self.ensure_no_scene_continuation(record.account_id, record.game)?;
        fsio::ensure_dir(&self.progression_dir(record.account_id.0))?;
        let mut stored = record.clone();
        stored.schema_version = SCHEMA_VERSION;
        self.write_character_record(&path, stored).map(|_| ())
    }

    /// Read-modify-write of the account's [`CharacterProgress`] for `game`, as one step that no
    /// other writer can interleave with: `change` gets the stored record (`None` if there is none
    /// yet) and returns the record to store, which is returned in turn. Two battles that end at
    /// once for one account therefore add up instead of one overwriting the other.
    ///
    /// `change` runs while the store's writer lock is held: it must be quick and must not call
    /// back into this store (the lock is not reentrant, so that would deadlock). What it returns
    /// must be for the same account and game and pass the checks of
    /// [`Store::put_character_progress`] (a bad inventory is one of them); otherwise nothing is
    /// written and the old record stays. What it returns **replaces the whole record**, the
    /// inventory included: build it from the record the closure was given, not from
    /// [`CharacterProgress::new`], or a stored inventory is dropped. Adding to an item that the
    /// inventory already lists means raising its count, not adding a second stack of it.
    pub fn update_character_progress<F>(
        &self,
        account: AccountId,
        game: GameId,
        change: F,
    ) -> Result<CharacterProgress>
    where
        F: FnOnce(Option<CharacterProgress>) -> CharacterProgress,
    {
        let path = self.character_path(account.0, game)?;

        let _counters = self.writer()?;
        let _progress = self.progress_write();
        if !self.index().contains_account(account.0) {
            return Err(PersistError::UnknownAccount(account));
        }
        self.ensure_no_scene_continuation(account, game)?;
        let mut next = change(self.read_character_progress(account, game)?);
        if next.account_id != account || next.game != game {
            return Err(PersistError::InvalidField {
                field: "account_id",
                reason: format!(
                    "the updated record is for account {:?} / {}, not the account {:?} / {} it \
                     replaces",
                    next.account_id,
                    game_key(next.game),
                    account,
                    game_key(game)
                ),
            });
        }
        next.validate()?;
        next.schema_version = SCHEMA_VERSION;
        fsio::ensure_dir(&self.progression_dir(account.0))?;
        self.write_character_record(&path, next)
    }

    /// Raise the recognized index fence to an explicitly selected minimum compatibility.
    /// This does not initialize, validate or register records, and cannot downgrade an
    /// existing store. Existing registration methods keep their own default requirements.
    /// Callers that need an atomic registration/write must establish its required fence
    /// inside that operation after validation and before any record becomes visible.
    /// An uncertain fence replacement blocks progress and writes until this Store reopens.
    pub fn require_compatibility(&self, minimum: index::StoreCompatibility) -> Result<()> {
        let counters = self.writer()?;
        let _progress = self.progress_write();
        self.ensure_index_format(&counters, minimum.index_version())
    }

    /// The caller already holds the writer and progress gates. Raise the recognized index
    /// header to version 4 before explicit durable story operations or schema-3 progress are
    /// acknowledged. Earlier version-3 writers lack the ordinary exact-story/battle commit
    /// and uncertain-write guarantees; they must refuse the entire store. The character and
    /// intent formats do not change. No account data changes.
    fn ensure_scene_batch_format(&self, counters: &Counters) -> Result<()> {
        self.ensure_index_format(counters, index::DURABLE_STORY_INDEX_VERSION)
    }

    /// The caller already holds the writer and progress gates. Protect visit metadata from
    /// earlier writers before its first durable write or successful acknowledgment.
    fn ensure_field_visit_format(&self, counters: &Counters) -> Result<()> {
        self.ensure_index_format(counters, index::FIELD_VISIT_INDEX_VERSION)
    }

    /// Select a supported minimum without changing records or lowering a prior fence.
    /// Both caller gates are already held. Validation rejects unknown output versions before
    /// the uncertainty latch; a legacy minimum of 2 leaves existing version-2/3 stores alone.
    fn ensure_index_format(&self, counters: &Counters, minimum: u32) -> Result<()> {
        self.ensure_transaction_healthy()?;
        if !(crate::model::INDEX_LOG_VERSION..=index::FIELD_VISIT_INDEX_VERSION).contains(&minimum) {
            return Err(PersistError::InvalidField { field: "store_compatibility", reason: "unsupported index compatibility minimum".into() });
        }
        let mut log = self.log();
        log.ensure_healthy()?;
        if self.index().format_version() >= minimum { return Ok(()); }
        let path = self.log_path();
        // atomic_write can report a directory-sync failure after replacing the log. Until
        // reopening decides which inode survived, no reader or writer may use this Store.
        self.transaction_poisoned.store(true, Ordering::Release);
        let upgraded = (|| {
            let (len, _) = index::write_compacted_version(&path, &self.index(), counters.next_account_id,
                counters.next_avatar_id, minimum)?;
            #[cfg(test)]
            if FAIL_SCENE_FORMAT_AFTER_RENAME.with(|flag| flag.replace(false)) {
                return Err(PersistError::io("injected scene format fence failure", &path,
                    std::io::Error::other("index replacement completed before writer reopen")));
            }
            let fresh = LogWriter::open(&path, len)?;
            *log = fresh;
            self.index_mut().require_format(minimum);
            Ok(())
        })();
        match upgraded {
            Ok(()) => { self.transaction_poisoned.store(false, Ordering::Release); Ok(()) }
            Err(error) => {
                log.poison(format!("the scene format fence needs reopen ({error})"));
                Err(error)
            }
        }
    }

    // ---- maintenance -------------------------------------------------------------------

    /// Rewrite `index.log` as the minimal log for the current accounts (one entry per account,
    /// plus the id high-water marks), dropping the entries that releases made dead. Done
    /// automatically when the store opens and the log has accumulated many of them; this is
    /// for an operator who wants it now. Registration waits while it runs (it holds the writer
    /// lock); logins are unaffected except an account's very first one.
    pub fn compact_index(&self) -> Result<()> {
        let counters = self.writer()?;
        let mut log = self.log();
        let path = self.log_path();
        let (len, _entries) = {
            let index = self.index();
            index::write_compacted(
                &path,
                &index,
                counters.next_account_id,
                counters.next_avatar_id,
            )?
        };
        // The old file is gone from the directory; appending to the old handle would lose data.
        match LogWriter::open(&path, len) {
            Ok(fresh) => {
                *log = fresh;
                Ok(())
            }
            Err(e) => {
                log.poison(format!("the compacted log could not be reopened ({e})"));
                Err(e)
            }
        }
    }

    // ---- backup ------------------------------------------------------------------------

    /// Copy the whole store into `dest` (which must be absent or empty, and outside the store)
    /// as a consistent point-in-time image. The writer lock is already held by this `Store`, so
    /// no other process can change files; the writer mutex keeps this process's writers out
    /// for the duration. The copy is itself a valid state directory (`Store::open` it, or use
    /// [`crate::restore_snapshot`]); `index.log` is written last, so an interrupted snapshot
    /// is recognizable as incomplete.
    ///
    /// Logins keep working while it runs, apart from an account's very first one in the
    /// moment the log is copied.
    pub fn snapshot(&self, dest: impl AsRef<Path>) -> Result<SnapshotInfo> {
        let _writers_excluded = self.writer()?;
        snapshot::snapshot_tree(&self.root, dest.as_ref(), &self.log)
    }
}

/// An avatar profile for a new account.
fn avatar_profile(
    account: AccountId,
    avatar: AvatarId,
    display_name: &str,
    new: &NewAvatar,
    now: u64,
) -> AvatarProfile {
    AvatarProfile {
        schema_version: SCHEMA_VERSION,
        avatar_id: avatar,
        account_id: account,
        display_name: display_name.to_owned(),
        tint: new.tint,
        base_archetype: new.base_archetype.clone(),
        per_game_overrides: new.per_game_overrides.clone(),
        created_at: now,
        updated_at: now,
    }
}

/// Sweep leftovers of interrupted writes from every store directory.
fn sweep_all_temp_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut removed = fsio::sweep_temp_files(root)?;
    removed.extend(fsio::sweep_temp_files(&root.join(ACCOUNTS_DIR))?);
    removed.extend(fsio::sweep_temp_files(&root.join(AVATARS_DIR))?);
    removed.extend(fsio::sweep_temp_files(&root.join(CHARACTERS_DIR))?);
    let progression = root.join(PROGRESSION_DIR);
    removed.extend(fsio::sweep_temp_files(&progression)?);
    for dir in fsio::list_subdirs(&progression)? {
        removed.extend(fsio::sweep_temp_files(&progression.join(dir))?);
    }
    // An interrupted manifest write inside a trash entry.
    let trash = root.join(trash::TRASH_DIR);
    for dir in fsio::list_subdirs(&trash)? {
        removed.extend(fsio::sweep_temp_files(&trash.join(dir))?);
    }
    Ok(removed)
}

/// True if any record file exists (used to refuse opening records without an index).
fn has_any_records(root: &Path) -> Result<bool> {
    for sub in [ACCOUNTS_DIR, AVATARS_DIR, CHARACTERS_DIR] {
        if !fsio::list_json_files(&root.join(sub))?.is_empty() {
            return Ok(true);
        }
    }
    let progression = root.join(PROGRESSION_DIR);
    for dir in fsio::list_subdirs(&progression)? {
        if !fsio::list_json_files(&progression.join(dir))?.is_empty() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Record files the index does not reference: what an interrupted creation (or the tail of an
/// interrupted release) leaves behind. They are ignored, and overwritten when their id is next
/// allocated. Nothing is deleted.
fn find_unreferenced_records(root: &Path, index: &Index) -> Result<Vec<String>> {
    let id_of = |file: &str| {
        file.strip_suffix(".json")
            .and_then(|s| s.parse::<u64>().ok())
    };
    let mut messages = Vec::new();

    let mut check = |dir: &str, known: &dyn Fn(u64) -> bool, what: &str| -> Result<()> {
        let unknown: Vec<String> = fsio::list_json_files(&root.join(dir))?
            .into_iter()
            .filter(|file| id_of(file).is_none_or(|id| !known(id)))
            .collect();
        if !unknown.is_empty() {
            messages.push(format!(
                "ignoring {} {what} record(s) not referenced by the index ({}); \
                 likely left by an interrupted creation or release",
                unknown.len(),
                describe_names(&unknown)
            ));
        }
        Ok(())
    };
    check(ACCOUNTS_DIR, &|id| index.contains_account(id), "account")?;
    check(AVATARS_DIR, &|id| index.contains_avatar(id), "avatar")?;
    check(CHARACTERS_DIR, &|id| index.contains_account(id), "player-character")?;

    let unknown: Vec<String> = fsio::list_subdirs(&root.join(PROGRESSION_DIR))?
        .into_iter()
        .filter(|dir| {
            dir.parse::<u64>()
                .map_or(true, |id| !index.contains_account(id))
        })
        .collect();
    if !unknown.is_empty() {
        messages.push(format!(
            "ignoring progression for {} unknown account(s) ({})",
            unknown.len(),
            describe_names(&unknown)
        ));
    }
    Ok(messages)
}

/// The first few names, then "and N more".
fn describe_names(names: &[String]) -> String {
    const LIMIT: usize = 5;
    let shown = names
        .iter()
        .take(LIMIT)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if names.len() > LIMIT {
        format!("{shown} and {} more", names.len() - LIMIT)
    } else {
        shown
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::legacy::describe_ids;
    use std::sync::mpsc;
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn an_interrupted_seeded_registration_cannot_give_its_progress_to_a_reused_id() {
        for reopen in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let mut store = Store::open(dir.path()).unwrap();
            let mut seed = NewCharacterProgress::new(GameId::Ff7, 1, 7);
            seed.gil = 240;
            seed.inventory = Some(vec![fmv_core::InventoryEntry::new(0, 3)]);
            let character = NewPlayerCharacter::for_game("body", GameId::Ff7,
                crate::GameCharacter { binding: Some("ff7/barret".into()), slot: Some(1), progress: crate::GameCharacter::pointers(GameId::Ff7, Some(1)) });
            FAIL_REGISTRATION_BEFORE_COMMIT.with(|flag| flag.set(true));
            assert!(store.register_with_character_and_progress("Lost", [1, 2, 3], "default", None, Some(character), Some(seed), 5).is_err());
            assert!(store.character_path(1, GameId::Ff7).unwrap().exists(), "the interruption is after the initial file was written");
            assert!(store.character_record_path(1).exists());
            assert_eq!(store.account_count(), 0, "the name/token never committed");
            if reopen { drop(store); store = Store::open(dir.path()).unwrap(); }
            let (account, _) = store.create_account("Next", 6).unwrap();
            assert_eq!(account, AccountId(1), "an uncommitted id may be reused");
            assert!(store.get_character_progress(account, GameId::Ff7).unwrap().is_none());
            assert!(store.get_player_character(account).unwrap().is_none());
        }
    }

    #[test]
    fn restoring_into_a_failed_registrations_id_cannot_inherit_its_seed_or_character() {
        for archived_game in [None, Some(GameId::Ff8), Some(GameId::Ff7)] {
            let dir = tempfile::tempdir().unwrap();
            let store = Store::open(dir.path()).unwrap();
            let (old, _) = store.create_account("Legacy", 1).unwrap();
            let mut old_receipt = None;
            if let Some(game) = archived_game {
                let mut record = CharacterProgress::new(old, game, 2, 4, 2);
                record.gil = 17;
                record.inventory = Some(vec![fmv_core::InventoryEntry::new(4, 8)]);
                store.put_character_progress(&record).unwrap();
                if game == GameId::Ff7 {
                    let before = store.get_progress_pair(old, game).unwrap();
                    let mut next = before.clone();
                    next.story.blob = vec![7];
                    next.character.as_mut().unwrap().gil = 18;
                    store.commit_scene_progress("archived-scene", &before, &next).unwrap();
                    old_receipt = Some((before, next));
                }
            }
            let deleted = store.delete_account(old, 3).unwrap();
            let mut seed = NewCharacterProgress::new(GameId::Ff7, 1, 7);
            seed.gil = 240;
            let character = NewPlayerCharacter::for_game("body", GameId::Ff7,
                crate::GameCharacter { binding: Some("ff7/barret".into()), slot: Some(1), progress: crate::GameCharacter::pointers(GameId::Ff7, Some(1)) });
            FAIL_REGISTRATION_BEFORE_COMMIT.with(|flag| flag.set(true));
            assert!(store.register_with_character_and_progress("Lost", [1, 2, 3], "default", None, Some(character), Some(seed), 4).is_err());
            assert!(store.character_path(2, GameId::Ff7).unwrap().exists());
            let restored = store.restore_account(&deleted.entry, 5).unwrap();
            assert_eq!(restored.account_id, AccountId(2));
            assert!(restored.unfinished.is_empty());
            assert_eq!(store.get_character_progress(restored.account_id, GameId::Ff7).unwrap().map(|r| r.gil),
                (archived_game == Some(GameId::Ff7)).then_some(18));
            assert!(store.get_player_character(restored.account_id).unwrap().is_none());
            assert_eq!(store.get_character_progress(restored.account_id, GameId::Ff8).unwrap().map(|r| r.gil),
                (archived_game == Some(GameId::Ff8)).then_some(17));
            if let Some((mut before, mut next)) = old_receipt {
                for pair in [&mut before, &mut next] {
                    pair.story.account_id = restored.account_id;
                    pair.character.as_mut().unwrap().account_id = restored.account_id;
                }
                assert_eq!(store.commit_scene_progress("archived-scene", &before, &next).unwrap(), crate::SceneCommit::AlreadyApplied);
                assert_eq!(store.get_character_progress(restored.account_id, GameId::Ff7).unwrap().unwrap().inventory,
                    Some(vec![fmv_core::InventoryEntry::new(4, 8)]));
            }
            drop(store);
            let reopened = Store::open(dir.path()).unwrap();
            assert_eq!(reopened.get_character_progress(restored.account_id, GameId::Ff7).unwrap().map(|r| r.gil),
                (archived_game == Some(GameId::Ff7)).then_some(18));
        }
    }

    #[test]
    fn an_uncertain_index_commit_prevents_seed_cleanup_by_registration_and_restore() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let (legacy, _) = store.create_account("Legacy", 1).unwrap();
        let deleted = store.delete_account(legacy, 2).unwrap();
        let (survivor, _) = store.create_account("Survivor", 2).unwrap();
        let mut seed = NewCharacterProgress::new(GameId::Ff7, 0, 6);
        seed.gil = 240;
        FAIL_REGISTRATION_BEFORE_COMMIT.with(|flag| flag.set(true));
        assert!(store.register_with_character_and_progress("Lost", [1, 2, 3], "default", None, None, Some(seed), 3).is_err());
        let path = store.character_path(3, GameId::Ff7).unwrap();
        let before = std::fs::read(&path).unwrap();
        let avatar_before = std::fs::read(store.avatar_path(1)).unwrap();
        // Model an append whose complete line reached disk but whose failure left the in-memory
        // index and next id unchanged. Neither allocator may erase that possibly committed seed.
        let account = store.load_account(3).unwrap();
        store.log().append(&Op::Account { id: 3, name: account.display_name, token: account.token_hash,
            at: 3, avatar: Some(1), claimed: false }).unwrap();
        store.log().poison("uncertain append".into());
        assert!(matches!(store.create_account("Next", 4), Err(PersistError::IndexPoisoned { .. })));
        assert!(matches!(store.restore_account(&deleted.entry, 4), Err(PersistError::IndexPoisoned { .. })));
        assert!(matches!(store.create_avatar(survivor, [4, 5, 6], "default", 4), Err(PersistError::IndexPoisoned { .. })));
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(std::fs::read(store.avatar_path(1)).unwrap(), avatar_before);
        drop(store);
        let reopened = Store::open(dir.path()).unwrap();
        assert_eq!(reopened.get_character_progress(AccountId(3), GameId::Ff7).unwrap().unwrap().gil, 240);
    }

    fn registration_story_fixture() -> NewProgression {
        NewProgression::new(GameId::Ff7, (0..5117).map(|n| (n % 251) as u8).collect())
    }

    #[test]
    fn partial_and_complete_initial_state_failures_never_claim_a_name_or_leak_into_a_reused_id() {
        for between_files in [false, true] {
            for reopen in [false, true] {
                let dir = tempfile::tempdir().unwrap();
                let mut store = Store::open(dir.path()).unwrap();
                let mut seed = NewCharacterProgress::new(GameId::Ff7, 0, 6);
                seed.gil = 240;
                seed.inventory = Some(vec![fmv_core::InventoryEntry::new(0, 3)]);
                if between_files { FAIL_REGISTRATION_AFTER_CHARACTER_SEED.with(|flag| flag.set(true)); }
                else { FAIL_REGISTRATION_BEFORE_COMMIT.with(|flag| flag.set(true)); }
                assert!(store.register_with_character_and_initial_state("Interrupted", [1, 2, 3], "default", None,
                    None, Some(seed), Some(registration_story_fixture()), 5).is_err());
                assert_eq!(store.account_count(), 0);
                assert!(store.character_path(1, GameId::Ff7).unwrap().exists());
                assert_eq!(store.progression_path(1, GameId::Ff7).unwrap().exists(), !between_files);
                if reopen { drop(store); store = Store::open(dir.path()).unwrap(); }
                let (account, _) = store.create_account("Interrupted", 6).unwrap();
                assert_eq!(account, AccountId(1));
                assert!(store.get_character_progress(account, GameId::Ff7).unwrap().is_none());
                assert!(store.get_progression(account, GameId::Ff7).unwrap().blob.is_empty());
                assert!(!store.progression_path(account.0, GameId::Ff7).unwrap().exists());
                assert_eq!(store.index().format_version(), index::DURABLE_STORY_INDEX_VERSION,
                    "an interrupted opt-in may safely retain the compatibility fence");
            }
        }
    }

    #[test]
    fn restore_allocation_replaces_both_uncommitted_seed_files_with_only_the_archived_state() {
        for archived in [false, true] {
            for between_files in [false, true] {
                let dir = tempfile::tempdir().unwrap();
                let store = Store::open(dir.path()).unwrap();
                let (old, _) = store.create_account("Archived", 1).unwrap();
                if archived {
                    store.put_progression(&Progression::new(old, GameId::Ff7, vec![9, 8, 7], 2)).unwrap();
                    let mut record = CharacterProgress::new(old, GameId::Ff7, 2, 5, 2);
                    record.gil = 17;
                    record.inventory = Some(vec![fmv_core::InventoryEntry::new(4, 8)]);
                    store.put_character_progress(&record).unwrap();
                }
                let deleted = store.delete_account(old, 3).unwrap();
                let mut seed = NewCharacterProgress::new(GameId::Ff7, 0, 6);
                seed.gil = 240;
                if between_files { FAIL_REGISTRATION_AFTER_CHARACTER_SEED.with(|flag| flag.set(true)); }
                else { FAIL_REGISTRATION_BEFORE_COMMIT.with(|flag| flag.set(true)); }
                assert!(store.register_with_character_and_initial_state("Interrupted", [1, 2, 3], "default", None,
                    None, Some(seed), Some(registration_story_fixture()), 4).is_err());
                let restored = store.restore_account(&deleted.entry, 5).unwrap();
                assert_eq!(restored.account_id, AccountId(2));
                assert!(restored.unfinished.is_empty());
                let check = |store: &Store| {
                    assert_eq!(store.get_progression(restored.account_id, GameId::Ff7).unwrap().blob,
                        if archived { vec![9, 8, 7] } else { vec![] });
                    let record = store.get_character_progress(restored.account_id, GameId::Ff7).unwrap();
                    assert_eq!(record.as_ref().map(|r| r.gil), archived.then_some(17));
                    if let Some(record) = record { assert_eq!(record.inventory, Some(vec![fmv_core::InventoryEntry::new(4, 8)])); }
                };
                check(&store);
                drop(store);
                check(&Store::open(dir.path()).unwrap());
            }
        }
    }

    #[test]
    fn an_uncertain_account_append_cannot_erase_either_initial_state_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let (legacy, _) = store.create_account("Archived", 1).unwrap();
        let deleted = store.delete_account(legacy, 2).unwrap();
        let mut seed = NewCharacterProgress::new(GameId::Ff7, 0, 6);
        seed.gil = 240;
        FAIL_REGISTRATION_BEFORE_COMMIT.with(|flag| flag.set(true));
        let story = registration_story_fixture();
        assert!(store.register_with_character_and_initial_state("Interrupted", [1, 2, 3], "default", None,
            None, Some(seed), Some(story.clone()), 3).is_err());
        let paths = [store.character_path(2, GameId::Ff7).unwrap(), store.progression_path(2, GameId::Ff7).unwrap()];
        let bytes: Vec<_> = paths.iter().map(|path| std::fs::read(path).unwrap()).collect();
        let account = store.load_account(2).unwrap();
        store.log().append(&Op::Account { id: 2, name: account.display_name, token: account.token_hash,
            at: 3, avatar: Some(1), claimed: false }).unwrap();
        store.log().poison("uncertain append".into());
        assert!(matches!(store.create_account("Next", 4), Err(PersistError::IndexPoisoned { .. })));
        assert!(matches!(store.restore_account(&deleted.entry, 4), Err(PersistError::IndexPoisoned { .. })));
        assert!(matches!(store.register_with_character_and_initial_state("Other", [1, 2, 3], "default", None,
            None, None, Some(story.clone()), 4), Err(PersistError::IndexPoisoned { .. })));
        for (path, expected) in paths.iter().zip(bytes) { assert_eq!(std::fs::read(path).unwrap(), expected); }
        drop(store);
        let reopened = Store::open(dir.path()).unwrap();
        assert_eq!(reopened.get_character_progress(AccountId(2), GameId::Ff7).unwrap().unwrap().gil, 240);
        assert_eq!(reopened.get_progression(AccountId(2), GameId::Ff7).unwrap().blob, story.blob);
        reopened.release_unclaimed(AccountId(2)).unwrap();
        assert_eq!(reopened.account_count(), 0, "the untouched undelivered pair remains releasable after recovery");
    }

    #[test]
    fn uncertain_initial_story_format_upgrade_writes_no_account_and_requires_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        FAIL_SCENE_FORMAT_AFTER_RENAME.with(|flag| flag.set(true));
        let story = registration_story_fixture();
        assert!(store.register_with_character_and_initial_state("New Story", [1, 2, 3], "default", None,
            None, None, Some(story.clone()), 1).is_err());
        assert_eq!(store.account_count(), 0);
        assert!(!store.account_path(1).exists());
        assert!(!store.progression_path(1, GameId::Ff7).unwrap().exists());
        assert!(matches!(store.create_account("Blocked", 2), Err(PersistError::TransactionRecoveryRequired)));
        assert!(matches!(store.get_progression(AccountId(1), GameId::Ff7), Err(PersistError::TransactionRecoveryRequired)));
        drop(store);
        let reopened = Store::open(dir.path()).unwrap();
        assert_eq!(reopened.index().format_version(), index::DURABLE_STORY_INDEX_VERSION);
        let (registered, character, initial) = reopened.register_with_character_and_initial_state("New Story", [1, 2, 3], "default", None,
            None, None, Some(story.clone()), 3).unwrap();
        assert_eq!(registered.account_id, AccountId(1));
        assert!(character.is_none());
        assert_eq!(initial.as_ref().unwrap().blob, story.blob);
        assert_eq!(reopened.get_progression(registered.account_id, GameId::Ff7).unwrap(), initial.unwrap());
    }

    #[test]
    fn store_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Store>();
    }

    #[test]
    fn tokens_are_long_prefixed_hex_and_distinct() {
        let a = new_token().unwrap();
        let b = new_token().unwrap();
        assert_ne!(a, b);
        for t in [&a, &b] {
            assert_eq!(t.len(), TOKEN_PREFIX.len() + 64);
            assert!(t.starts_with(TOKEN_PREFIX));
            assert!(t[TOKEN_PREFIX.len()..]
                .bytes()
                .all(|c| c.is_ascii_hexdigit()));
        }
    }

    #[test]
    fn token_hash_is_plain_blake3() {
        assert_eq!(token_hash("abc"), *blake3::hash(b"abc").as_bytes());
        assert_eq!(describe_ids(&[1, 2]), "1, 2");
        assert_eq!(
            describe_ids(&[1, 2, 3, 4, 5, 6, 7]),
            "1, 2, 3, 4, 5 and 2 more"
        );
    }

    /// Run `work` on another thread and fail unless it finishes while the caller still holds
    /// whatever lock the test took.
    fn finishes_promptly<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(work());
        });
        rx.recv_timeout(Duration::from_secs(20))
            .expect("the call was blocked by a lock it must not need")
    }

    #[test]
    fn reads_never_wait_for_the_writer_lock() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path().join("state")).unwrap());
        let reg = store
            .register("Cloud", [1, 2, 3], "default", None, 5)
            .unwrap();
        // The first login claims the account; do it once so the checks below are the ordinary
        // path (the first login itself is covered by the next test).
        store.authenticate(&reg.token).unwrap();
        store.create_account("Tifa", 5).unwrap();

        // A registration (or any writer) holds the writer lock for its whole duration. Nothing a
        // login does may need it.
        let held = store.writer().unwrap();
        let s = Arc::clone(&store);
        let token = reg.token.clone();
        finishes_promptly(move || {
            assert_eq!(s.authenticate(&token).unwrap(), reg.account_id);
            assert!(matches!(
                s.authenticate("fmvt_nope"),
                Err(PersistError::InvalidToken)
            ));
            assert_eq!(s.find_account_by_name("cloud"), Some(reg.account_id));
            assert_eq!(
                s.account_info(reg.account_id)
                    .unwrap()
                    .unwrap()
                    .display_name,
                "Cloud"
            );
            assert!(s.avatar_for_account(reg.account_id).unwrap().is_some());
            assert!(s.get_avatar(reg.avatar.avatar_id).unwrap().is_some());
            assert!(s
                .get_progression(reg.account_id, GameId::Ff7)
                .unwrap()
                .blob
                .is_empty());
            assert_eq!(s.account_count(), 2);
            assert_eq!(s.unclaimed_accounts().len(), 1);
        });
        drop(held);
    }

    #[test]
    fn first_login_needs_the_log_lock_but_not_the_writer_lock() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path().join("state")).unwrap());
        let (account, token) = store.create_account("Cid", 5).unwrap();
        assert_eq!(store.unclaimed_accounts().len(), 1);

        let held = store.writer().unwrap();
        let s = Arc::clone(&store);
        let logged_in = finishes_promptly(move || s.authenticate(&token).unwrap());
        assert_eq!(logged_in, account);
        drop(held);
        assert!(store.unclaimed_accounts().is_empty());
    }

    #[test]
    fn scene_batch_format_fence_preserves_records_and_survives_reopen_compaction_and_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("live");
        let store = Store::open(&root).unwrap();
        let (account, token) = store.create_account("Fence", 1).unwrap();
        let mut character = CharacterProgress::new(account, GameId::Ff7, 0, 6, 2);
        character.gil = 240;
        character.inventory = Some(vec![]);
        store.put_character_progress(&character).unwrap();
        let bytes = std::fs::read(store.character_path(account.0, GameId::Ff7).unwrap()).unwrap();
        assert_eq!(store.index().format_version(), crate::model::INDEX_LOG_VERSION);
        {
            let counters = store.writer().unwrap();
            let _progress = store.progress_write();
            store.ensure_scene_batch_format(&counters).unwrap();
            store.ensure_scene_batch_format(&counters).unwrap();
        }
        assert_eq!(store.index().format_version(), index::DURABLE_STORY_INDEX_VERSION);
        assert_eq!(std::fs::read(store.character_path(account.0, GameId::Ff7).unwrap()).unwrap(), bytes);
        // Authentication must append to the replaced log's new inode, not its old handle.
        assert_eq!(store.authenticate(&token).unwrap(), account);
        store.compact_index().unwrap();
        let snapshot = dir.path().join("snapshot");
        store.snapshot(&snapshot).unwrap();
        drop(store);
        let reopened = Store::open(&root).unwrap();
        assert_eq!(reopened.index().format_version(), index::DURABLE_STORY_INDEX_VERSION);
        assert!(reopened.unclaimed_accounts().is_empty(), "the post-fence claim was durable");
        assert_eq!(reopened.get_character_progress(account, GameId::Ff7).unwrap(), Some(character.clone()));
        let restored = dir.path().join("restored");
        crate::restore_snapshot(&snapshot, &restored).unwrap();
        let restored = Store::open(&restored).unwrap();
        assert_eq!(restored.index().format_version(), index::DURABLE_STORY_INDEX_VERSION);
        assert_eq!(restored.get_character_progress(account, GameId::Ff7).unwrap(), Some(character));
    }

    #[test]
    fn scene_batch_format_uncertain_replacement_fences_all_progress_until_reopen() {
        for prior_version in [crate::model::INDEX_LOG_VERSION, index::SCENE_BATCH_INDEX_VERSION] {
            let dir = tempfile::tempdir().unwrap();
            let store = Store::open(dir.path()).unwrap();
            let (account, _) = store.create_account("Fence Failure", 1).unwrap();
            drop(store);
            // A valid earlier-version store can reach the same uncertain upgrade point.
            let path = dir.path().join(index::INDEX_LOG_FILE);
            let previous = index::load_log(&path, false).unwrap().unwrap();
            index::write_compacted_version(&path, &previous.index, previous.next_account_id,
                previous.next_avatar_id, prior_version).unwrap();
            let store = Store::open(dir.path()).unwrap();
            assert_eq!(store.index().format_version(), prior_version);
            {
                let counters = store.writer().unwrap();
                let _progress = store.progress_write();
                FAIL_SCENE_FORMAT_AFTER_RENAME.with(|flag| flag.set(true));
                assert!(store.ensure_scene_batch_format(&counters).is_err());
            }
            assert!(matches!(store.get_progression(account, GameId::Ff7), Err(PersistError::TransactionRecoveryRequired)));
            assert!(matches!(store.put_progression(&Progression::empty(account, GameId::Ff7)), Err(PersistError::TransactionRecoveryRequired)));
            assert!(matches!(store.create_account("Blocked", 2), Err(PersistError::TransactionRecoveryRequired)));
            drop(store);
            let reopened = Store::open(dir.path()).unwrap();
            assert_eq!(reopened.index().format_version(), index::DURABLE_STORY_INDEX_VERSION);
            assert_eq!(reopened.account_count(), 1);
            assert!(reopened.get_progression(account, GameId::Ff7).unwrap().blob.is_empty());
        }
    }

}
