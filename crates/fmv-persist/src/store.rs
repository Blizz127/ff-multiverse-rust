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
//! <root>/progression/<account_id>/<Game>.json   Progression (the story blob, also the phase key)
//! <root>/progression/<account_id>/<Game>.character.json   CharacterProgress (level, exp, ap, gil)
//! ```
//!
//! # Crash consistency
//!
//! `index.log` is the single commit point for everything that allocates an id or must stay
//! unique. Creating an account (with its avatar) writes the record files first (nothing
//! references them yet) and appends **one** log entry last: the name, the token and the ids
//! become real together, or not at all. A crash in between leaves unreferenced record files
//! that the next creation overwrites, never a dangling reference or a reused id. Updates to
//! an existing avatar or progression are one atomic file write each.
//!
//! # Locks
//!
//! Three locks, always taken in this order when more than one is needed:
//!
//! 1. **writer** (a `Mutex`): held by every operation that changes anything on disk
//!    (registration, avatar and progression writes, release, compaction, snapshots) for its
//!    whole duration. That keeps [`Store::snapshot`] a true point-in-time copy and makes
//!    "check that the name is free, then take it" one step.
//! 2. **log** (a `Mutex`): held only while an entry is appended and synced, and by the first
//!    login of an account, which records that it happened. Short, and never held across
//!    anything but that one write.
//! 3. **index** (an `RwLock`): the in-memory lookup tables. Readers hold it for a hash lookup;
//!    a writer takes it for the few microseconds it needs to insert after the entry is durable.
//!
//! Everything a login needs ([`Store::authenticate`] and the lookups after it) only reads the
//! index, so **a login never waits for a registration** or any other disk write, however long
//! it takes. The one exception is the very first login of an account, which has to append its
//! `claim` entry and so waits for the log lock, that is, for at most one other entry's sync.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

use fmv_core::{name_key, AccountId, AvatarAppearance, GameId};

use crate::error::{PersistError, Result};
use crate::fsio;
use crate::index::{self, AccountEntry, Index, LogWriter, Op, TokenHash, INDEX_LOG_FILE};
use crate::lock::LockGuard;
use crate::model::{
    check_new_appearance_ref, game_key, is_safe_key, AccountInfo, AccountRecord, AvatarId,
    AvatarProfile, CharacterProgress, Progression, Registered, MAX_BLOB_BYTES,
    MAX_CHARACTER_RECORD_BYTES, MAX_RECORD_BYTES, MAX_TOKEN_BYTES, SCHEMA_VERSION,
};
use crate::snapshot::{self, SnapshotInfo};

pub(crate) const ACCOUNTS_DIR: &str = "accounts";
pub(crate) const AVATARS_DIR: &str = "avatars";
pub(crate) const PROGRESSION_DIR: &str = "progression";
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
    /// Lock 2.
    log: Mutex<LogWriter>,
    /// Lock 3.
    index: RwLock<Index>,
    warnings: Vec<String>,
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

        for sub in [ACCOUNTS_DIR, AVATARS_DIR, PROGRESSION_DIR] {
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

        for message in find_unreferenced_records(&root, &opened.index)? {
            tracing::warn!("{message}");
            warnings.push(message);
        }

        Ok(Store {
            root,
            max_accounts: options.max_accounts,
            writer: Mutex::new(Counters {
                next_account_id: opened.next_account_id,
                next_avatar_id: opened.next_avatar_id,
            }),
            log: Mutex::new(opened.log),
            index: RwLock::new(opened.index),
            warnings,
            _lock: lock,
        })
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
    fn writer(&self) -> MutexGuard<'_, Counters> {
        self.writer.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Lock 2.
    fn log(&self) -> MutexGuard<'_, LogWriter> {
        self.log.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Lock 3, shared.
    fn index(&self) -> RwLockReadGuard<'_, Index> {
        self.index.read().unwrap_or_else(PoisonError::into_inner)
    }

    /// Lock 3, exclusive: only after the matching log entry is durable.
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
    pub fn register(
        &self,
        display_name: &str,
        tint: [u8; 3],
        base_archetype: &str,
        appearance_ref: Option<(GameId, &str)>,
        now: u64,
    ) -> Result<Registered> {
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

        self.check_capacity()?;
        let token = new_token()?;
        let (account_id, profile) =
            self.insert_account(name, token_hash(&token), now, Some(avatar))?;
        let avatar = profile.ok_or_else(|| {
            PersistError::corrupt(&self.log_path(), "a registration produced no avatar")
        })?;
        Ok(Registered {
            account_id,
            token,
            avatar,
        })
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
        let (account, _) = self.insert_account(name, token_hash(&token), now, None)?;
        Ok((account, token))
    }

    /// The write path shared by `register` and `create_account`: take the writer lock, check
    /// that the name is free, write the record files, append the log entry.
    fn insert_account(
        &self,
        name: String,
        hash: TokenHash,
        now: u64,
        avatar: Option<NewAvatar>,
    ) -> Result<(AccountId, Option<AvatarProfile>)> {
        let key = name_key(&name);
        let mut counters = self.writer();
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

        // 1. The records. Nothing references them yet.
        let record = AccountRecord {
            schema_version: SCHEMA_VERSION,
            account_id: AccountId(id),
            display_name: name.clone(),
            token_hash: hex::encode(hash),
            created_at: now,
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
        Ok((AccountId(id), profile))
    }

    /// Resolve a token to its account. Unknown, malformed and oversized tokens all fail with
    /// [`PersistError::InvalidToken`].
    ///
    /// Only reads the in-memory index, so it never waits for a registration or any other
    /// write. The first successful login of an account also records, durably, that its token
    /// has been used (which is what keeps `release_unclaimed` away from it); that is one
    /// appended log entry, and a failure to write it never fails the login.
    pub fn authenticate(&self, token: &str) -> Result<AccountId> {
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
        let mut log = self.log();
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
    /// [`Store::unclaimed_accounts`]) and that has no stored progression; otherwise it fails
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
        let _counters = self.writer();
        let avatar = self
            .index()
            .account(account.0)
            .ok_or(PersistError::UnknownAccount(account))?
            .avatar;
        let progression_dir = self.progression_dir(account.0);
        if !fsio::list_json_files(&progression_dir)?.is_empty() {
            return Err(PersistError::NotReleasable {
                account,
                reason: "it has stored progression",
            });
        }
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
        let mut leftovers = vec![self.account_path(account.0)];
        leftovers.extend(avatar.map(|id| self.avatar_path(id)));
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

    /// [`Store::release_unclaimed`] for a caller that releases on a best-effort basis, for
    /// example a server that could not deliver the reply of a registration: `Ok(true)` when the
    /// account was released, `Ok(false)` when there is nothing to release (the account is
    /// unknown, its token has been used, or it has stored progression), `Err` only when the
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
        let mut counters = self.writer();
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
        let _counters = self.writer();
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
        if !self.index().contains_account(account.0) {
            return Err(PersistError::UnknownAccount(account));
        }
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
    /// untouched. The blob is capped at [`MAX_BLOB_BYTES`].
    pub fn put_progression(&self, progression: &Progression) -> Result<()> {
        if progression.blob.len() > MAX_BLOB_BYTES {
            return Err(PersistError::BlobTooLarge {
                len: progression.blob.len(),
                max: MAX_BLOB_BYTES,
            });
        }
        let path = self.progression_path(progression.account_id.0, progression.game)?;

        let _counters = self.writer();
        if !self.index().contains_account(progression.account_id.0) {
            return Err(PersistError::UnknownAccount(progression.account_id));
        }
        fsio::ensure_dir(&self.progression_dir(progression.account_id.0))?;
        let mut stored = progression.clone();
        stored.schema_version = SCHEMA_VERSION;
        fsio::write_json(&path, &stored, MAX_RECORD_BYTES)
    }

    // ---- character progress ------------------------------------------------------------

    /// The account's [`CharacterProgress`] for `game`, or `None` if none was ever stored (the
    /// caller starts from its game's own starting values; nothing is migrated). It is a separate
    /// record from [`Store::get_progression`]'s story blob. Fails with
    /// [`PersistError::UnknownAccount`] for an account that does not exist, and with
    /// [`PersistError::Corrupt`] / [`PersistError::FutureSchema`] / [`PersistError::TooLarge`]
    /// for a file that is damaged, written by a newer build or oversized: such a file is never
    /// replaced by a default behind the caller's back.
    ///
    /// Takes no writer lock: a write replaces the file atomically, so this sees the whole old
    /// record or the whole new one, and a login never waits for a battle result being saved.
    pub fn get_character_progress(
        &self,
        account: AccountId,
        game: GameId,
    ) -> Result<Option<CharacterProgress>> {
        if !self.index().contains_account(account.0) {
            return Err(PersistError::UnknownAccount(account));
        }
        self.read_character_progress(account, game)
    }

    fn read_character_progress(
        &self,
        account: AccountId,
        game: GameId,
    ) -> Result<Option<CharacterProgress>> {
        let path = self.character_path(account.0, game)?;
        let Some(record) = fsio::read_json::<CharacterProgress>(&path, MAX_CHARACTER_RECORD_BYTES)?
        else {
            return Ok(None);
        };
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
    /// untouched. The record must pass the checks every game shares (`level` at least 1; the
    /// game's own caps are its adapter's business) and the account must exist. Prefer
    /// [`Store::update_character_progress`] when the new value depends on the old one.
    pub fn put_character_progress(&self, record: &CharacterProgress) -> Result<()> {
        record.validate()?;
        let path = self.character_path(record.account_id.0, record.game)?;

        let _counters = self.writer();
        if !self.index().contains_account(record.account_id.0) {
            return Err(PersistError::UnknownAccount(record.account_id));
        }
        fsio::ensure_dir(&self.progression_dir(record.account_id.0))?;
        let mut stored = record.clone();
        stored.schema_version = SCHEMA_VERSION;
        fsio::write_json(&path, &stored, MAX_CHARACTER_RECORD_BYTES)
    }

    /// Read-modify-write of the account's [`CharacterProgress`] for `game`, as one step that no
    /// other writer can interleave with: `change` gets the stored record (`None` if there is none
    /// yet) and returns the record to store, which is returned in turn. Two battles that end at
    /// once for one account therefore add up instead of one overwriting the other.
    ///
    /// `change` runs while the store's writer lock is held: it must be quick and must not call
    /// back into this store (the lock is not reentrant, so that would deadlock). What it returns
    /// must be for the same account and game and pass the checks of
    /// [`Store::put_character_progress`]; otherwise nothing is written and the old record stays.
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

        let _counters = self.writer();
        if !self.index().contains_account(account.0) {
            return Err(PersistError::UnknownAccount(account));
        }
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
        fsio::write_json(&path, &next, MAX_CHARACTER_RECORD_BYTES)?;
        Ok(next)
    }

    // ---- maintenance -------------------------------------------------------------------

    /// Rewrite `index.log` as the minimal log for the current accounts (one entry per account,
    /// plus the id high-water marks), dropping the entries that releases made dead. Done
    /// automatically when the store opens and the log has accumulated many of them; this is
    /// for an operator who wants it now. Registration waits while it runs (it holds the writer
    /// lock); logins are unaffected except an account's very first one.
    pub fn compact_index(&self) -> Result<()> {
        let counters = self.writer();
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
        let _writers_excluded = self.writer();
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
    let progression = root.join(PROGRESSION_DIR);
    removed.extend(fsio::sweep_temp_files(&progression)?);
    for dir in fsio::list_subdirs(&progression)? {
        removed.extend(fsio::sweep_temp_files(&progression.join(dir))?);
    }
    Ok(removed)
}

/// True if any record file exists (used to refuse opening records without an index).
fn has_any_records(root: &Path) -> Result<bool> {
    for sub in [ACCOUNTS_DIR, AVATARS_DIR] {
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
        let held = store.writer();
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

        let held = store.writer();
        let s = Arc::clone(&store);
        let logged_in = finishes_promptly(move || s.authenticate(&token).unwrap());
        assert_eq!(logged_in, account);
        drop(held);
        assert!(store.unclaimed_accounts().is_empty());
    }
}
