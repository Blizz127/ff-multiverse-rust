//! The store: accounts, avatar profiles and per-game progression under one state directory.
//!
//! ```text
//! <root>/LOCK                              single-writer guard (pid)
//! <root>/index.json                        names, token hashes, avatar links, next ids
//! <root>/accounts/<account_id>.json        AccountRecord
//! <root>/avatars/<avatar_id>.json          AvatarProfile
//! <root>/progression/<account_id>/<Game>.json   Progression
//! ```
//!
//! Crash consistency without a journal: `index.json` is the single commit point for
//! everything that allocates an id or must stay unique. Creating an account or avatar writes
//! the new record file first (nothing references it yet) and the index last. A crash in
//! between leaves an unreferenced file that the next creation overwrites, never a dangling
//! reference or a reused id. Updates to an existing avatar or progression are one atomic file
//! write each.
//!
//! Every writer, including ones that never touch the index, holds the state mutex for its whole
//! operation. That makes [`Store::snapshot`] a true point-in-time copy.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use fmv_core::{AccountId, AvatarAppearance, GameId};
use serde::{Deserialize, Serialize};

use crate::error::{PersistError, Result};
use crate::fsio;
use crate::lock::LockGuard;
use crate::model::{
    game_key, is_safe_key, AccountInfo, AccountRecord, AvatarId, AvatarProfile, Progression,
    MAX_BLOB_BYTES, MAX_INDEX_BYTES, MAX_RECORD_BYTES, MAX_TOKEN_BYTES, SCHEMA_VERSION,
};
use crate::snapshot::{self, SnapshotInfo};

pub(crate) const INDEX_FILE: &str = "index.json";
pub(crate) const ACCOUNTS_DIR: &str = "accounts";
pub(crate) const AVATARS_DIR: &str = "avatars";
pub(crate) const PROGRESSION_DIR: &str = "progression";
const TOKEN_PREFIX: &str = "fmvt_";
/// How many unreferenced records a warning names before saying "and N more".
const WARNING_LIST_LIMIT: usize = 5;

/// `index.json`. Everything a lookup needs without reading record files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct IndexFile {
    pub schema_version: u32,
    pub next_account_id: u64,
    pub next_avatar_id: u64,
    /// Lowercase display name -> account id. Exactly one entry per account.
    #[serde(default)]
    pub names: BTreeMap<String, u64>,
    /// blake3(token) hex -> account id.
    #[serde(default)]
    pub tokens: BTreeMap<String, u64>,
    /// Account id -> avatar id. At most one avatar per account.
    #[serde(default)]
    pub avatars: BTreeMap<u64, u64>,
}

impl IndexFile {
    fn fresh() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            next_account_id: 1,
            next_avatar_id: 1,
            names: BTreeMap::new(),
            tokens: BTreeMap::new(),
            avatars: BTreeMap::new(),
        }
    }

    /// Cheap structural checks so a damaged index is an error instead of a later surprise
    /// (a reused id, a token that points nowhere).
    pub(crate) fn validate(&self, path: &Path) -> Result<()> {
        let bad = |reason: String| Err(PersistError::corrupt(path, reason));
        if self.next_account_id == 0 || self.next_avatar_id == 0 {
            return bad("next ids must start at 1".into());
        }
        let mut accounts = BTreeSet::new();
        for (name, &id) in &self.names {
            if name.is_empty() || id == 0 || id >= self.next_account_id {
                return bad(format!("name {name:?} maps to invalid account id {id}"));
            }
            if !accounts.insert(id) {
                return bad(format!("account {id} has more than one name"));
            }
        }
        for (hash, &id) in &self.tokens {
            if !is_token_hash(hash) || !accounts.contains(&id) {
                return bad(format!("token entry {hash:?} -> {id} is invalid"));
            }
        }
        let mut avatars = BTreeSet::new();
        for (&account, &avatar) in &self.avatars {
            if !accounts.contains(&account) || avatar == 0 || avatar >= self.next_avatar_id {
                return bad(format!("avatar link {account} -> {avatar} is invalid"));
            }
            if !avatars.insert(avatar) {
                return bad(format!(
                    "avatar {avatar} is linked to more than one account"
                ));
            }
        }
        Ok(())
    }
}

fn is_token_hash(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// In-memory copy of the index plus the id sets derived from it.
struct State {
    index: IndexFile,
    account_ids: BTreeSet<u64>,
    avatar_ids: BTreeSet<u64>,
}

impl State {
    fn from_index(index: IndexFile) -> Self {
        let account_ids = index.names.values().copied().collect();
        let avatar_ids = index.avatars.values().copied().collect();
        Self {
            index,
            account_ids,
            avatar_ids,
        }
    }
}

/// Handle to one state directory. Holds the single-writer lock until dropped.
///
/// All methods take `&self` and the type is `Send + Sync`, so it can be shared as
/// `Arc<Store>`. Calls do blocking file I/O (including fsync); async callers should use
/// `spawn_blocking`.
pub struct Store {
    root: PathBuf,
    state: Mutex<State>,
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

fn name_key(sanitized: &str) -> String {
    sanitized.to_lowercase()
}

fn token_hash(token: &str) -> String {
    blake3::hash(token.as_bytes()).to_hex().to_string()
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

fn describe_ids(ids: &[String]) -> String {
    let shown = ids
        .iter()
        .take(WARNING_LIST_LIMIT)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if ids.len() > WARNING_LIST_LIMIT {
        format!("{shown} and {} more", ids.len() - WARNING_LIST_LIMIT)
    } else {
        shown
    }
}

impl Store {
    /// Open (creating if needed) the state directory at `root` and take its writer lock.
    ///
    /// Fails with [`PersistError::Locked`] if another live process holds the lock. A lock left
    /// by a dead process is taken over; that and other recoverable oddities are reported
    /// through `tracing::warn!` and [`Store::open_warnings`].
    pub fn open(root: impl AsRef<Path>) -> Result<Store> {
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

        let index_path = root.join(INDEX_FILE);
        let index = match fsio::read_json::<IndexFile>(&index_path, MAX_INDEX_BYTES)? {
            Some(index) => {
                index.validate(&index_path)?;
                index
            }
            None => {
                if has_any_records(&root)? {
                    return Err(PersistError::IndexMissing { root });
                }
                let index = IndexFile::fresh();
                fsio::write_json(&index_path, &index, MAX_INDEX_BYTES)?;
                index
            }
        };
        let state = State::from_index(index);

        for message in find_unreferenced_records(&root, &state)? {
            tracing::warn!("{message}");
            warnings.push(message);
        }

        Ok(Store {
            root,
            state: Mutex::new(state),
            warnings,
            _lock: lock,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Recoverable oddities noticed while opening (stale lock taken over, temp files swept,
    /// unreferenced records left by a crash).
    pub fn open_warnings(&self) -> &[String] {
        &self.warnings
    }

    // ---- paths -----------------------------------------------------------------------

    fn index_path(&self) -> PathBuf {
        self.root.join(INDEX_FILE)
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

    // ---- shared plumbing ---------------------------------------------------------------

    /// Poisoning cannot leave the state half-updated (it only changes after a successful
    /// disk write), so recover the guard instead of panicking.
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Persist `next` as the new index and, only on success, adopt it in memory. If the write
    /// fails, re-sync memory with whatever is actually on disk.
    fn commit_index(&self, state: &mut State, next: IndexFile) -> Result<()> {
        let path = self.index_path();
        match fsio::write_json(&path, &next, MAX_INDEX_BYTES) {
            Ok(()) => {
                *state = State::from_index(next);
                Ok(())
            }
            Err(e) => {
                if let Ok(Some(disk)) = fsio::read_json::<IndexFile>(&path, MAX_INDEX_BYTES) {
                    if disk.validate(&path).is_ok() {
                        *state = State::from_index(disk);
                    }
                }
                Err(e)
            }
        }
    }

    fn load_account(&self, id: u64) -> Result<AccountRecord> {
        let path = self.account_path(id);
        let record: AccountRecord = fsio::read_json(&path, MAX_RECORD_BYTES)?.ok_or_else(|| {
            PersistError::corrupt(&path, "referenced by index.json but the file is missing")
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
                PersistError::corrupt(&path, "referenced by index.json but the file is missing")
            })?;
        if profile.avatar_id.0 != id {
            return Err(PersistError::corrupt(
                &path,
                format!("record claims avatar id {}", profile.avatar_id.0),
            ));
        }
        Ok(profile)
    }

    // ---- accounts ----------------------------------------------------------------------

    /// Create an account. Returns its id and the plaintext token, which is shown exactly once;
    /// only `blake3(token)` is stored.
    ///
    /// `display_name` is cleaned with `AvatarAppearance::sanitize_name` and must be unique
    /// case-insensitively. `now` is the caller's unix time in seconds.
    pub fn create_account(&self, display_name: &str, now: u64) -> Result<(AccountId, String)> {
        let name =
            AvatarAppearance::sanitize_name(display_name).ok_or(PersistError::InvalidName)?;
        let key = name_key(&name);
        let token = new_token()?;
        let hash = token_hash(&token);

        let mut state = self.state();
        if state.index.names.contains_key(&key) {
            return Err(PersistError::NameTaken(name));
        }
        if state.index.tokens.contains_key(&hash) {
            // 256 random bits colliding means the entropy source is broken. Never overwrite
            // another account's token.
            return Err(PersistError::Entropy("token collision".into()));
        }
        let id = state.index.next_account_id;
        let next_id = id.checked_add(1).ok_or(PersistError::IdExhausted)?;

        let record = AccountRecord {
            schema_version: SCHEMA_VERSION,
            account_id: AccountId(id),
            display_name: name,
            token_hash: hash.clone(),
            created_at: now,
        };
        // 1. The record. Nothing references it yet.
        fsio::write_json(&self.account_path(id), &record, MAX_RECORD_BYTES)?;
        // 2. The commit point.
        let mut next = state.index.clone();
        next.next_account_id = next_id;
        next.names.insert(key, id);
        next.tokens.insert(hash, id);
        self.commit_index(&mut state, next)?;
        Ok((AccountId(id), token))
    }

    /// Resolve a token to its account. Unknown, malformed and oversized tokens all fail with
    /// [`PersistError::InvalidToken`].
    pub fn authenticate(&self, token: &str) -> Result<AccountId> {
        if token.len() > MAX_TOKEN_BYTES {
            return Err(PersistError::InvalidToken);
        }
        let hash = token_hash(token);
        self.state()
            .index
            .tokens
            .get(&hash)
            .map(|&id| AccountId(id))
            .ok_or(PersistError::InvalidToken)
    }

    /// `Ok(None)` for an unknown account.
    pub fn account_info(&self, id: AccountId) -> Result<Option<AccountInfo>> {
        if !self.state().account_ids.contains(&id.0) {
            return Ok(None);
        }
        let record = self.load_account(id.0)?;
        Ok(Some(AccountInfo {
            account_id: id,
            display_name: record.display_name,
            created_at: record.created_at,
        }))
    }

    /// Look an account up by display name (sanitized and compared case-insensitively).
    pub fn find_account_by_name(&self, display_name: &str) -> Option<AccountId> {
        let name = AvatarAppearance::sanitize_name(display_name)?;
        self.state()
            .index
            .names
            .get(&name_key(&name))
            .map(|&id| AccountId(id))
    }

    // ---- avatars -----------------------------------------------------------------------

    /// Create the account's avatar. Its display name is the account's. One avatar per account.
    pub fn create_avatar(
        &self,
        account: AccountId,
        tint: [u8; 3],
        base_archetype: &str,
        now: u64,
    ) -> Result<AvatarProfile> {
        let mut state = self.state();
        if !state.account_ids.contains(&account.0) {
            return Err(PersistError::UnknownAccount(account));
        }
        if state.index.avatars.contains_key(&account.0) {
            return Err(PersistError::AvatarExists(account));
        }
        let display_name = self.load_account(account.0)?.display_name;
        let id = state.index.next_avatar_id;
        let next_id = id.checked_add(1).ok_or(PersistError::IdExhausted)?;

        let profile = AvatarProfile {
            schema_version: SCHEMA_VERSION,
            avatar_id: AvatarId(id),
            account_id: account,
            display_name,
            tint,
            base_archetype: base_archetype.to_owned(),
            per_game_overrides: BTreeMap::new(),
            created_at: now,
            updated_at: now,
        };
        profile.validate_fields()?;
        // 1. The record. 2. The commit point.
        fsio::write_json(&self.avatar_path(id), &profile, MAX_RECORD_BYTES)?;
        let mut next = state.index.clone();
        next.next_avatar_id = next_id;
        next.avatars.insert(account.0, id);
        self.commit_index(&mut state, next)?;
        Ok(profile)
    }

    /// Replace an existing avatar with `profile`, which must carry the avatar id and account id
    /// it was loaded with. `updated_at` is the caller's. `display_name` must still match the
    /// account's name (case-insensitively); renames are not supported yet.
    pub fn put_avatar(&self, profile: &AvatarProfile) -> Result<()> {
        profile.validate_fields()?;
        let name = AvatarAppearance::sanitize_name(&profile.display_name)
            .ok_or(PersistError::InvalidName)?;

        // Held across the write so snapshots and other writers see whole operations only.
        let state = self.state();
        match state.index.avatars.get(&profile.account_id.0) {
            Some(&id) if id == profile.avatar_id.0 => {}
            _ => return Err(PersistError::UnknownAvatar(profile.avatar_id)),
        }
        if state.index.names.get(&name_key(&name)) != Some(&profile.account_id.0) {
            return Err(PersistError::InvalidField {
                field: "display_name",
                reason: "must match the account's display name (case-insensitively); \
                         renaming is not supported yet"
                    .into(),
            });
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
        if !self.state().avatar_ids.contains(&id.0) {
            return Ok(None);
        }
        self.load_avatar(id.0).map(Some)
    }

    /// The account's avatar, if it has one.
    pub fn avatar_for_account(&self, account: AccountId) -> Result<Option<AvatarProfile>> {
        let linked = self.state().index.avatars.get(&account.0).copied();
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
        if !self.state().account_ids.contains(&account.0) {
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

        let state = self.state();
        if !state.account_ids.contains(&progression.account_id.0) {
            return Err(PersistError::UnknownAccount(progression.account_id));
        }
        fsio::ensure_dir(&self.progression_dir(progression.account_id.0))?;
        let mut stored = progression.clone();
        stored.schema_version = SCHEMA_VERSION;
        fsio::write_json(&path, &stored, MAX_RECORD_BYTES)
    }

    // ---- backup ------------------------------------------------------------------------

    /// Copy the whole store into `dest` (which must be absent or empty, and outside the store)
    /// as a consistent point-in-time image. The writer lock is already held by this `Store`, so
    /// no other process can change files; the state mutex keeps this process's writers out
    /// for the duration. The copy is itself a valid state directory (`Store::open` it, or use
    /// [`crate::restore_snapshot`]); `index.json` is written last, so an interrupted snapshot
    /// is recognizable as incomplete.
    pub fn snapshot(&self, dest: impl AsRef<Path>) -> Result<SnapshotInfo> {
        let _writers_excluded = self.state();
        snapshot::snapshot_tree(&self.root, dest.as_ref())
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

/// Record files the index does not reference: what an interrupted creation leaves behind.
/// They are ignored, and overwritten when their id is next allocated. Nothing is deleted.
fn find_unreferenced_records(root: &Path, state: &State) -> Result<Vec<String>> {
    let id_of = |file: &str| {
        file.strip_suffix(".json")
            .and_then(|s| s.parse::<u64>().ok())
    };
    let mut messages = Vec::new();

    let mut check = |dir: &str, known: &BTreeSet<u64>, what: &str| -> Result<()> {
        let unknown: Vec<String> = fsio::list_json_files(&root.join(dir))?
            .into_iter()
            .filter(|file| id_of(file).is_none_or(|id| !known.contains(&id)))
            .collect();
        if !unknown.is_empty() {
            messages.push(format!(
                "ignoring {} {what} record(s) not referenced by index.json ({}); \
                 likely left by an interrupted creation",
                unknown.len(),
                describe_ids(&unknown)
            ));
        }
        Ok(())
    };
    check(ACCOUNTS_DIR, &state.account_ids, "account")?;
    check(AVATARS_DIR, &state.avatar_ids, "avatar")?;

    let unknown: Vec<String> = fsio::list_subdirs(&root.join(PROGRESSION_DIR))?
        .into_iter()
        .filter(|dir| {
            dir.parse::<u64>()
                .map_or(true, |id| !state.account_ids.contains(&id))
        })
        .collect();
    if !unknown.is_empty() {
        messages.push(format!(
            "ignoring progression for {} unknown account(s) ({})",
            unknown.len(),
            describe_ids(&unknown)
        ));
    }
    Ok(messages)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn token_hash_is_plain_blake3_hex() {
        assert_eq!(token_hash("abc"), blake3::hash(b"abc").to_hex().to_string());
        assert!(is_token_hash(&token_hash("abc")));
        assert!(!is_token_hash("ABC"));
        assert!(!is_token_hash(&"A".repeat(64)));
    }

    #[test]
    fn index_validation_catches_damage() {
        let path = Path::new("index.json");
        let mut ok = IndexFile::fresh();
        ok.next_account_id = 3;
        ok.next_avatar_id = 2;
        ok.names.insert("a".into(), 1);
        ok.names.insert("b".into(), 2);
        ok.tokens.insert(token_hash("t1"), 1);
        ok.avatars.insert(2, 1);
        ok.validate(path).unwrap();

        let broken: [&dyn Fn(&mut IndexFile); 8] = [
            &|i| i.next_account_id = 0,
            &|i| i.next_avatar_id = 0,
            &|i| {
                i.names.insert("c".into(), 3); // id >= next_account_id
            },
            &|i| {
                i.names.insert("c".into(), 1); // two names, one account
            },
            &|i| {
                i.tokens.insert("nothex".into(), 1);
            },
            &|i| {
                i.tokens.insert(token_hash("t2"), 9); // unknown account
            },
            &|i| {
                i.avatars.insert(1, 5); // avatar id beyond next_avatar_id
            },
            &|i| {
                i.avatars.insert(1, 1); // avatar 1 linked twice
            },
        ];
        for (n, damage) in broken.iter().enumerate() {
            let mut idx = ok.clone();
            damage(&mut idx);
            assert!(
                matches!(idx.validate(path), Err(PersistError::Corrupt { .. })),
                "case {n}"
            );
        }
    }
}
