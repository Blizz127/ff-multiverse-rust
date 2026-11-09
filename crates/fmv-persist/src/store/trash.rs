//! Soft delete and restore: the trash of the store.
//!
//! A player can delete the character they made: the account, its avatar, its player-character record and its
//! progress. The store never throws any of that away. [`Store::delete_account`] moves the account's files into a
//! **trash entry** inside the state directory and frees the account's name; [`Store::restore_account`] brings an
//! entry back, as a new account (ids are never reused, see below).
//!
//! ```text
//! <root>/trash/<stamp>-<account>/                     a deleted account, complete: restorable
//!     manifest.json                                   who it was, when it was deleted, the files the entry holds
//!     accounts/<account>.json                         the records exactly as they were, in the layout of the
//!     avatars/<avatar>.json                           state directory (so an operator can also copy a file back
//!     characters/<account>.json                       by hand)
//!     progression/<account>/<Game>.json
//!     progression/<account>/<Game>.character.json
//! <root>/trash/<stamp>-<account>.pending/             a delete in flight
//! <root>/trash/<stamp>-<account>.restoring-<new>/     a restore in flight
//! <root>/trash/<stamp>-<account>.restored-<new>/      restored as account <new> (kept as the record of it)
//! ```
//!
//! `<stamp>` is the time of the delete in UTC, `YYYYMMDDTHHMMSSZ`, so entries sort as time does. The state of an
//! entry is its directory name, and every change of state is one atomic `rename` of that directory.
//!
//! # What a delete changes
//!
//! Only the files of the one account: its account record, its avatar profile (the one the index links it to), its
//! player-character record and every record under its own `progression/<account>/`. It removes the account from
//! every index exactly as releasing an orphan does (`Op::Release`): the name is free and can be registered again,
//! the token is void, the avatar link is gone, and the account id is retired for good (ids only grow, so none is
//! ever handed out again). It never reads, writes or moves a file of any other account.
//!
//! # Crash consistency (delete)
//!
//! `index.log` is the commit point, as everywhere in this store. Three steps, in this order:
//!
//! 1. `<entry>.pending/manifest.json` is written atomically and lists the files to move. Nothing in the live
//!    directories has changed and the account still exists: a crash here leaves a plan nobody acts on.
//! 2. **One** synced `release` entry is appended to `index.log`: the account, its token and its name leave the
//!    index together, or not at all.
//! 3. The listed files are renamed into the entry, one atomic rename each, and the entry is renamed to its final
//!    name (that last rename says "complete").
//!
//! Opening the store settles what a crash left, after the index is replayed and before anything else looks at the
//! records ([`recover`]): a `.pending` entry whose account is **still in the index** was never committed, so it is
//! removed and the account is intact; one whose account is **gone from the index** was committed, so the moves are
//! finished (a rename that already happened is skipped) and the entry sealed. A crash between the steps therefore
//! never leaves a name taken without its character, a character without its name, or a record in two places for
//! good. The same recovery runs for a delete whose moves failed after the commit (the caller is told, and the next
//! open finishes them).
//!
//! # Restore
//!
//! A restore **re-registers** the character: the name must still be free, the account gets a **new** account id
//! (and a new avatar id), because the old ids are retired and the index refuses to replay an id that does not
//! exceed the ones before it. Every record that carries an id is re-keyed (account, avatar, player character, and
//! every progress record), the account gets a **new token** (returned once, only its hash is stored: the old
//! token went with the client that deleted the character), and the new account is `claimed` (it is not an
//! orphan, whatever progress it holds). The old entry is renamed to `.restored-<new>`, so it cannot be restored a
//! second time. The order mirrors a registration: the records that nothing references yet first, then the one
//! synced `account` entry that makes it real, then the progress records (a registration has none to write), then
//! the rename that says "complete". A crash after the commit leaves a `.restoring-<new>` entry whose account is
//! in the index: opening finishes it (progress records that already exist are never overwritten). A crash before
//! it leaves one whose account is not: opening puts the entry back and removes the unreferenced records.
//!
//! Snapshots (`Store::snapshot`) are rollback images of the live records and do not copy the trash.

use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use fmv_core::{name_key, AccountId};
use serde::{Deserialize, Serialize};

use super::{new_token, token_hash, Store, ACCOUNTS_DIR, AVATARS_DIR, CHARACTERS_DIR, PROGRESSION_DIR};
use crate::character::{PlayerCharacter, MAX_PLAYER_CHARACTER_BYTES};
use crate::error::{PersistError, Result};
use crate::fsio;
use crate::index::{AccountEntry, Index, Op};
use crate::model::{
    is_safe_key, AccountRecord, AvatarId, AvatarProfile, CharacterProgress, Progression, MAX_CHARACTER_RECORD_BYTES,
    MAX_RECORD_BYTES, SCHEMA_VERSION,
};

/// The directory of the trash inside the state directory. Made by the first delete, not when the store opens, so a
/// state directory that never had a delete is byte for byte what it was.
pub(crate) const TRASH_DIR: &str = "trash";

const MANIFEST_FILE: &str = "manifest.json";
/// The newest manifest version this build writes and reads.
const MANIFEST_VERSION: u32 = 1;
const MANIFEST_KIND: &str = "fmv-trash-entry";
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
/// An account, an avatar, a player character and two records per game: far under this.
const MAX_ENTRY_FILES: usize = 64;
const PENDING: &str = ".pending";
const RESTORING: &str = ".restoring-";
const RESTORED: &str = ".restored-";

// ---- what the store reports ----------------------------------------------------------------------------------------

/// What [`Store::delete_account`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deleted {
    pub account_id: AccountId,
    /// The name that is free again.
    pub display_name: String,
    /// The trash entry: a directory name under `<root>/trash/`.
    pub entry: String,
    /// What the entry holds: the record files, as paths relative to the entry (and to the state directory they
    /// came from).
    pub files: Vec<String>,
    /// Files the move could not finish after the commit (empty for an ordinary delete). The delete itself
    /// happened (the name is free, the token void); the next open of the store finishes these.
    pub unfinished: Vec<String>,
}

/// What [`Store::restore_account`] did.
#[derive(Clone, PartialEq, Eq)]
pub struct Restored {
    /// The restored account's NEW id.
    pub account_id: AccountId,
    pub display_name: String,
    /// The new account's token, shown exactly once: only `blake3(token)` is stored. `Debug` does not print it.
    pub token: String,
    /// The NEW avatar id (`None` if the entry held no avatar profile: the first login makes one).
    pub avatar_id: Option<AvatarId>,
    /// The entry it came from, as it is called now (`...restored-<new id>`).
    pub entry: String,
    /// Progress records the restore could not write after the commit (empty normally); the next open writes them.
    pub unfinished: Vec<String>,
}

impl fmt::Debug for Restored {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Restored")
            .field("account_id", &self.account_id)
            .field("display_name", &self.display_name)
            .field("token", &"<redacted>")
            .field("avatar_id", &self.avatar_id)
            .field("entry", &self.entry)
            .field("unfinished", &self.unfinished)
            .finish()
    }
}

/// Where a trash entry stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrashState {
    /// Deleted and complete: [`Store::restore_account`] can bring it back.
    Deleted,
    /// A delete in flight, or one a crash interrupted: the next open of the store finishes or abandons it.
    Pending,
    /// A restore in flight, or one a crash interrupted: the next open of the store finishes or undoes it.
    Restoring { as_account: AccountId },
    /// Restored as this account. Kept as the record of it; it cannot be restored again.
    Restored { as_account: AccountId },
}

/// One entry of the trash, as [`Store::trash_entries`] lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashEntry {
    /// The directory name under `<root>/trash/`: what [`Store::restore_account`] takes.
    pub name: String,
    pub state: TrashState,
    /// The account that was deleted (its id then; it is retired now).
    pub account_id: AccountId,
    pub display_name: String,
    /// Unix seconds.
    pub deleted_at: u64,
    /// `deleted_at` as UTC text (`2026-11-09T16:59:05Z`), as the manifest says it.
    pub deleted_at_utc: String,
    /// How many record files the entry holds.
    pub files: usize,
    /// Why the manifest could not be read (then `display_name`, `deleted_at` and `files` are placeholders).
    pub problem: Option<String>,
}

// ---- the manifest ----------------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Manifest {
    schema_version: u32,
    kind: String,
    account_id: u64,
    display_name: String,
    /// The avatar the index linked to the account when it was deleted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    avatar_id: Option<u64>,
    deleted_at: u64,
    /// `deleted_at` as UTC text, for people reading the file.
    deleted_at_utc: String,
    /// The files the entry holds (or is being given), relative paths with `/`.
    files: Vec<String>,
    /// Written before a restore starts, so that a crash can be undone or finished.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    restore: Option<RestorePlan>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct RestorePlan {
    account_id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    avatar_id: Option<u64>,
    at: u64,
}

fn read_manifest(dir: &Path) -> Result<Manifest> {
    let path = dir.join(MANIFEST_FILE);
    let bytes = fsio::read_capped(&path, MAX_MANIFEST_BYTES)?
        .ok_or_else(|| PersistError::corrupt(&path, "the trash entry has no manifest"))?;
    fsio::check_schema_version_upto(&path, &bytes, MANIFEST_VERSION)?;
    let manifest: Manifest =
        serde_json::from_slice(&bytes).map_err(|e| PersistError::corrupt(&path, format!("invalid manifest: {e}")))?;
    if manifest.kind != MANIFEST_KIND {
        return Err(PersistError::corrupt(&path, format!("kind is {:?}, not {MANIFEST_KIND:?}", manifest.kind)));
    }
    if manifest.files.len() > MAX_ENTRY_FILES {
        return Err(PersistError::corrupt(&path, format!("lists {} files, over the {MAX_ENTRY_FILES} limit", manifest.files.len())));
    }
    // A manifest names files that are moved by path: it may only name records of its own account.
    if let Some(bad) = manifest.files.iter().find(|rel| !owns(manifest.account_id, manifest.avatar_id, rel)) {
        return Err(PersistError::corrupt(&path, format!("lists {bad:?}, which is not a record of account {}", manifest.account_id)));
    }
    Ok(manifest)
}

fn write_manifest(dir: &Path, manifest: &Manifest) -> Result<()> {
    fsio::write_json(&dir.join(MANIFEST_FILE), manifest, MAX_MANIFEST_BYTES)
}

// ---- names and paths -----------------------------------------------------------------------------------------------

fn account_rel(account: u64) -> String {
    format!("{ACCOUNTS_DIR}/{account}.json")
}

fn avatar_rel(avatar: u64) -> String {
    format!("{AVATARS_DIR}/{avatar}.json")
}

fn character_rel(account: u64) -> String {
    format!("{CHARACTERS_DIR}/{account}.json")
}

fn progression_rel(account: u64, file: &str) -> String {
    format!("{PROGRESSION_DIR}/{account}/{file}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProgressKind {
    /// `<Game>.json`: the story blob.
    Story,
    /// `<Game>.character.json`: level, experience, items.
    Character,
}

/// What a file name inside `progression/<account>/` is: the game key and the kind of record.
fn progression_kind(file: &str) -> Option<(&str, ProgressKind)> {
    let stem = file.strip_suffix(".json")?;
    let (key, kind) = match stem.strip_suffix(".character") {
        Some(key) => (key, ProgressKind::Character),
        None => (stem, ProgressKind::Story),
    };
    is_safe_key(key).then_some((key, kind))
}

/// Is `rel` a record of this account (or of the avatar it was linked to)? The only paths a manifest may list.
fn owns(account: u64, avatar: Option<u64>, rel: &str) -> bool {
    if rel == account_rel(account) || rel == character_rel(account) {
        return true;
    }
    if avatar.is_some_and(|avatar| rel == avatar_rel(avatar)) {
        return true;
    }
    rel.strip_prefix(&format!("{PROGRESSION_DIR}/{account}/")).and_then(progression_kind).is_some()
}

/// All digits and at most 20 of them (so `+5`, ` 5` and `5 ` are not numbers here).
fn digits(text: &str) -> Option<u64> {
    if text.is_empty() || text.len() > 20 || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

fn is_stamp(text: &str) -> bool {
    text.len() == 16
        && text.bytes().enumerate().all(|(at, b)| match at {
            8 => b == b'T',
            15 => b == b'Z',
            _ => b.is_ascii_digit(),
        })
}

struct ParsedName<'a> {
    /// `<stamp>-<account>`.
    base: &'a str,
    account: u64,
    state: TrashState,
}

fn parse_name(name: &str) -> Option<ParsedName<'_>> {
    let (base, state) = if let Some(base) = name.strip_suffix(PENDING) {
        (base, TrashState::Pending)
    } else if let Some((base, new)) = name.rsplit_once(RESTORING) {
        (base, TrashState::Restoring { as_account: AccountId(digits(new)?) })
    } else if let Some((base, new)) = name.rsplit_once(RESTORED) {
        (base, TrashState::Restored { as_account: AccountId(digits(new)?) })
    } else {
        (name, TrashState::Deleted)
    };
    let (stamp, account) = base.rsplit_once('-')?;
    if !is_stamp(stamp) {
        return None;
    }
    Some(ParsedName { base, account: digits(account)?, state })
}

/// `YYYYMMDDTHHMMSSZ` for a unix time in seconds (UTC, proleptic Gregorian). A time past the year 292 billion
/// cannot be given and reads as 0.
pub(crate) fn utc_stamp(secs: u64) -> String {
    let (y, mo, d, h, mi, s) = utc_parts(secs);
    format!("{y:04}{mo:02}{d:02}T{h:02}{mi:02}{s:02}Z")
}

/// `YYYY-MM-DDTHH:MM:SSZ`, the same instant for people (the manifest's `deleted_at_utc`).
fn utc_text(secs: u64) -> String {
    let (y, mo, d, h, mi, s) = utc_parts(secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

fn utc_parts(secs: u64) -> (i64, i64, i64, u64, u64, u64) {
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rest = secs % 86_400;
    // Howard Hinnant's `civil_from_days`.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era = (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 { month_index + 3 } else { month_index - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day, rest / 3_600, rest % 3_600 / 60, rest % 60)
}

// ---- file operations ------------------------------------------------------------------------------------------------

fn remove_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

fn sync_all_dirs(dirs: &BTreeSet<PathBuf>) {
    for dir in dirs {
        // Best effort: every step below can be repeated, and a recovery is how a lost rename is found.
        let _ = fsio::sync_dir(dir);
    }
}

/// What finishing the moves of a delete found.
struct Moves {
    /// A rename or a check that failed: the entry is not sealed, and the next open tries again.
    failed: Vec<String>,
}

/// Move every file `manifest` lists from the state directory `root` into the entry `dir`. Idempotent: a file that
/// is no longer in the state directory but is in the entry was moved already; one that is in neither is not there
/// (something else removed it) and is let go. The empty `progression/<account>` directory is removed.
fn move_into_entry(root: &Path, dir: &Path, manifest: &Manifest) -> Moves {
    let mut failed = Vec::new();
    let mut touched: BTreeSet<PathBuf> = BTreeSet::new();
    for rel in &manifest.files {
        let (live, dest) = (root.join(rel), dir.join(rel));
        // `symlink_metadata`: a planted link is never followed, and never moved as if it were a record.
        match fs::symlink_metadata(&live) {
            Ok(meta) if meta.is_file() => {
                let moved = match dest.parent() {
                    Some(parent) => fsio::ensure_dir(parent).map_err(|e| e.to_string()),
                    None => Ok(()),
                }
                .and_then(|()| fs::rename(&live, &dest).map_err(|e| e.to_string()));
                match moved {
                    Ok(()) => {
                        touched.extend(live.parent().map(Path::to_path_buf));
                        touched.extend(dest.parent().map(Path::to_path_buf));
                    }
                    Err(why) => failed.push(format!("{rel}: {why}")),
                }
            }
            Ok(_) => failed.push(format!("{rel}: not a regular file")),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => failed.push(format!("{rel}: {e}")),
        }
    }
    let progression = root.join(PROGRESSION_DIR).join(manifest.account_id.to_string());
    // Empty, or it stays (anything else in it is not a record of the store and is not ours to remove).
    if fs::remove_dir(&progression).is_ok() {
        touched.extend(progression.parent().map(Path::to_path_buf));
    }
    sync_all_dirs(&touched);
    Moves { failed }
}

/// Rename `from` to `to` inside the trash and make it durable.
fn rename_entry(trash: &Path, from: &Path, to: &Path) -> Result<()> {
    fs::rename(from, to).map_err(|e| PersistError::io("rename trash entry", to, e))?;
    fsio::sync_dir(trash).map_err(|e| PersistError::io("fsync directory", trash, e))
}

// ---- what an entry holds, loaded and checked ---------------------------------------------------------------------

/// The records of an entry, read and checked (before a restore writes anything).
struct EntryRecords {
    account: AccountRecord,
    avatar: Option<AvatarProfile>,
    character: Option<PlayerCharacter>,
}

fn load_records(dir: &Path, manifest: &Manifest) -> Result<EntryRecords> {
    let id = manifest.account_id;
    let account_path = dir.join(account_rel(id));
    let account: AccountRecord = fsio::read_json(&account_path, MAX_RECORD_BYTES)?
        .ok_or_else(|| PersistError::corrupt(&account_path, "the trash entry holds no account record"))?;
    if account.account_id.0 != id {
        return Err(PersistError::corrupt(&account_path, format!("record claims account id {}", account.account_id.0)));
    }
    let avatar = match manifest.avatar_id.filter(|avatar| manifest.files.iter().any(|f| *f == avatar_rel(*avatar))) {
        Some(avatar_id) => {
            let path = dir.join(avatar_rel(avatar_id));
            let profile: AvatarProfile = fsio::read_json(&path, MAX_RECORD_BYTES)?
                .ok_or_else(|| PersistError::corrupt(&path, "the manifest lists it but it is not in the entry"))?;
            if profile.avatar_id.0 != avatar_id || profile.account_id.0 != id {
                return Err(PersistError::corrupt(&path, "the avatar profile does not belong to this account"));
            }
            profile.validate_fields().map_err(|e| PersistError::corrupt(&path, e.to_string()))?;
            Some(profile)
        }
        None => None,
    };
    let character = if manifest.files.iter().any(|f| *f == character_rel(id)) {
        let path = dir.join(character_rel(id));
        let record: PlayerCharacter = fsio::read_json(&path, MAX_PLAYER_CHARACTER_BYTES)?
            .ok_or_else(|| PersistError::corrupt(&path, "the manifest lists it but it is not in the entry"))?;
        if record.account_id.0 != id {
            return Err(PersistError::corrupt(&path, format!("record claims account {}", record.account_id.0)));
        }
        record.validate().map_err(|e| PersistError::corrupt(&path, e.to_string()))?;
        Some(record)
    } else {
        None
    };
    // The progress records are only checked here (they are written after the commit, by `write_progress`): a
    // restore that would fail on one of them must fail before it changes anything.
    for rel in manifest.files.iter().filter(|rel| rel.starts_with(PROGRESSION_DIR)) {
        read_progress(&dir.join(rel), rel, id)?;
    }
    Ok(EntryRecords { account, avatar, character })
}

/// A progress record of an entry (story blob or character progress), checked against the account it was filed under.
enum Progress {
    Story(Progression),
    Character(CharacterProgress),
}

fn read_progress(path: &Path, rel: &str, account: u64) -> Result<Progress> {
    let file = rel.rsplit('/').next().unwrap_or(rel);
    let Some((key, kind)) = progression_kind(file) else {
        return Err(PersistError::corrupt(path, "not a progress record of the store"));
    };
    let known = |game: fmv_core::GameId| crate::model::game_key(game) == key;
    match kind {
        ProgressKind::Story => {
            let record: Progression = fsio::read_json(path, MAX_RECORD_BYTES)?
                .ok_or_else(|| PersistError::corrupt(path, "the manifest lists it but it is not in the entry"))?;
            if record.account_id.0 != account || !known(record.game) {
                return Err(PersistError::corrupt(path, "the record is not for this account and the game its name says"));
            }
            Ok(Progress::Story(record))
        }
        ProgressKind::Character => {
            let record: CharacterProgress = fsio::read_json(path, MAX_CHARACTER_RECORD_BYTES)?
                .ok_or_else(|| PersistError::corrupt(path, "the manifest lists it but it is not in the entry"))?;
            if record.account_id.0 != account || !known(record.game) {
                return Err(PersistError::corrupt(path, "the record is not for this account and the game its name says"));
            }
            record.validate().map_err(|e| PersistError::corrupt(path, e.to_string()))?;
            Ok(Progress::Character(record))
        }
    }
}

/// Write the entry's progress records under `new_account`, re-keyed. A record that already exists is left alone
/// (a recovery repeats this, and a server may have saved to the restored account since). Returns what failed.
fn write_progress(root: &Path, dir: &Path, manifest: &Manifest, new_account: u64) -> Vec<String> {
    let mut failed = Vec::new();
    let target_dir = root.join(PROGRESSION_DIR).join(new_account.to_string());
    for rel in manifest.files.iter().filter(|rel| rel.starts_with(PROGRESSION_DIR)) {
        let file = rel.rsplit('/').next().unwrap_or(rel);
        let target = target_dir.join(file);
        if target.exists() {
            continue;
        }
        let written = read_progress(&dir.join(rel), rel, manifest.account_id).and_then(|record| {
            fsio::ensure_dir(&target_dir)?;
            match record {
                Progress::Story(mut record) => {
                    record.account_id = AccountId(new_account);
                    fsio::write_json(&target, &record, MAX_RECORD_BYTES)
                }
                Progress::Character(mut record) => {
                    record.account_id = AccountId(new_account);
                    fsio::write_json(&target, &record, MAX_CHARACTER_RECORD_BYTES)
                }
            }
        });
        if let Err(e) = written {
            failed.push(format!("{rel}: {e}"));
        }
    }
    failed
}

/// Remove the records a restore wrote before its commit (account, avatar and player character under the new ids):
/// what an undone restore leaves behind. Best effort; they are unreferenced either way.
fn remove_unreferenced_restore_records(root: &Path, plan: &RestorePlan) {
    let mut paths = vec![root.join(account_rel(plan.account_id)), root.join(character_rel(plan.account_id))];
    paths.extend(plan.avatar_id.map(|avatar| root.join(avatar_rel(avatar))));
    for path in paths {
        let _ = remove_if_exists(&path);
    }
}

// ---- opening the store: settle what a crash left ---------------------------------------------------------------------

/// Settle the trash entries a crash (or a failed move) left in flight. Runs when the store opens, after the index is
/// replayed (`index`, `next_account_id`) and before anything else looks at the records. Only acts on entries whose
/// account is in the state the entry says: see the module documentation. Whatever it does is a warning, and a
/// failure to settle one entry is a warning too, not a reason to refuse to open the store (the records it concerns
/// are unreferenced, and the next open tries again).
pub(super) fn recover(root: &Path, index: &Index, next_account_id: u64, warnings: &mut Vec<String>) -> Result<()> {
    let trash = root.join(TRASH_DIR);
    for name in fsio::list_subdirs(&trash)? {
        let Some(parsed) = parse_name(&name) else { continue };
        let dir = trash.join(&name);
        let outcome = match parsed.state {
            TrashState::Deleted | TrashState::Restored { .. } => continue,
            TrashState::Pending => recover_pending(root, &trash, &dir, &parsed, index, next_account_id),
            TrashState::Restoring { as_account } => recover_restoring(root, &trash, &dir, &parsed, as_account, index),
        };
        match outcome {
            Ok(Some(message)) => {
                tracing::warn!("{message}");
                warnings.push(message);
            }
            Ok(None) => {}
            Err(e) => {
                let message = format!("could not settle the trash entry {name}: {e}; it is left as it is and the next open tries again");
                tracing::warn!("{message}");
                warnings.push(message);
            }
        }
    }
    Ok(())
}

fn recover_pending(root: &Path, trash: &Path, dir: &Path, parsed: &ParsedName<'_>, index: &Index, next_account_id: u64) -> Result<Option<String>> {
    let account = parsed.account;
    if index.contains_account(account) {
        // The release was never committed (the manifest is written before it, so it would be here if it had been,
        // and a torn or missing one only says the plan never got that far): the account is intact and the entry
        // is only a plan.
        let who = read_manifest(dir).map_or_else(|_| String::new(), |manifest| format!(" ({:?})", manifest.display_name));
        fs::remove_dir_all(dir).map_err(|e| PersistError::io("remove abandoned trash entry", dir, e))?;
        return Ok(Some(format!(
            "abandoned an interrupted delete of account {account}{who}: it was never committed, so the account is intact"
        )));
    }
    let manifest = read_manifest(dir)?;
    if manifest.account_id != account {
        return Err(PersistError::corrupt(dir, "the manifest is for another account than the entry's name says"));
    }
    if account >= next_account_id {
        return Err(PersistError::corrupt(dir, "the account was never part of this index"));
    }
    // Committed: finish the moves and seal the entry.
    let moves = move_into_entry(root, dir, &manifest);
    if !moves.failed.is_empty() {
        return Err(PersistError::corrupt(dir, format!("the moves could not be finished: {}", moves.failed.join("; "))));
    }
    rename_entry(trash, dir, &trash.join(parsed.base))?;
    Ok(Some(format!(
        "finished an interrupted delete of account {account} ({:?}): its records are in the trash entry {}, its name is free",
        manifest.display_name, parsed.base
    )))
}

fn recover_restoring(
    root: &Path,
    trash: &Path,
    dir: &Path,
    parsed: &ParsedName<'_>,
    as_account: AccountId,
    index: &Index,
) -> Result<Option<String>> {
    let manifest = read_manifest(dir)?;
    let plan = manifest
        .restore
        .filter(|plan| plan.account_id == as_account.0)
        .ok_or_else(|| PersistError::corrupt(dir, "the entry is being restored but its manifest holds no matching plan"))?;
    if index.contains_account(as_account.0) {
        // The restore committed: write the progress records that are missing and seal the entry.
        let failed = write_progress(root, dir, &manifest, as_account.0);
        if !failed.is_empty() {
            return Err(PersistError::corrupt(dir, format!("the progress records could not be written: {}", failed.join("; "))));
        }
        rename_entry(trash, dir, &trash.join(format!("{}{RESTORED}{}", parsed.base, as_account.0)))?;
        return Ok(Some(format!(
            "finished an interrupted restore of {:?} as account {}",
            manifest.display_name, as_account.0
        )));
    }
    // It never committed: put the entry back and remove the records nothing references.
    remove_unreferenced_restore_records(root, &plan);
    let mut restorable = manifest;
    restorable.restore = None;
    write_manifest(dir, &restorable)?;
    rename_entry(trash, dir, &trash.join(parsed.base))?;
    Ok(Some(format!(
        "undid an interrupted restore of {:?}: it was never committed, so the entry is back in the trash",
        restorable.display_name
    )))
}

// ---- the store's operations ------------------------------------------------------------------------------------------

/// An entry that passed every check of a restore (see `check_restorable`).
struct Checked {
    /// `<stamp>-<account>`.
    base: String,
    dir: PathBuf,
    manifest: Manifest,
    records: EntryRecords,
    name: String,
    key: String,
}

/// A delete that has its plan on disk: the entry (still `.pending`) and what it lists.
struct Prepared {
    base: String,
    dir: PathBuf,
    manifest: Manifest,
}

impl Store {
    fn trash_dir(&self) -> PathBuf {
        self.root.join(TRASH_DIR)
    }

    /// Delete an account: move its records into a trash entry, free its name and remove it from every index.
    ///
    /// Only the files of this one account move (see the module documentation for the list and for the crash
    /// argument); nothing of any other account is read or written. The account's token stops working, its name can
    /// be registered again at once, and its id is retired for good. Nothing is thrown away: the entry
    /// ([`Deleted::entry`]) holds every file exactly as it was, and [`Store::restore_account`] brings it back.
    ///
    /// The store knows nothing of who is connected: a server that must not delete an account while it is being
    /// played refuses before it calls this. Fails with [`PersistError::UnknownAccount`] for an account that does
    /// not exist (also one that was deleted a moment ago). `now` is the caller's unix time in seconds.
    pub fn delete_account(&self, account: AccountId, now: u64) -> Result<Deleted> {
        // Held for the whole delete: no other write, and no snapshot, sees it half done.
        let _counters = self.writer();
        let prepared = self.prepare_delete(account, now)?;
        if let Err(e) = self.commit_delete(account) {
            // Nothing was committed: the entry is only a plan. (If this fails, the next open abandons it.)
            let _ = fs::remove_dir_all(&prepared.dir);
            return Err(e);
        }
        Ok(self.finish_delete(prepared))
    }

    /// The account's name, its avatar link and the record files it owns (the ones that exist, as paths relative to the
    /// state directory): exactly what a delete moves.
    fn collect_files(&self, account: AccountId) -> Result<(String, Option<u64>, Vec<String>)> {
        let (name, avatar) = {
            let index = self.index();
            let entry = index.account(account.0).ok_or(PersistError::UnknownAccount(account))?;
            (entry.name.clone(), entry.avatar)
        };
        let mut files = Vec::new();
        let mut push_if_file = |rel: String| {
            if fs::symlink_metadata(self.root.join(&rel)).is_ok_and(|meta| meta.is_file()) {
                files.push(rel);
            }
        };
        push_if_file(account_rel(account.0));
        if let Some(avatar) = avatar {
            push_if_file(avatar_rel(avatar));
        }
        push_if_file(character_rel(account.0));
        for file in fsio::list_json_files(&self.progression_dir(account.0))? {
            if progression_kind(&file).is_some() {
                push_if_file(progression_rel(account.0, &file));
            }
        }
        Ok((name, avatar, files))
    }

    /// The files [`Store::delete_account`] would move for `account`, as paths relative to the state directory: for a
    /// dry run. Reads only.
    pub fn deletion_plan(&self, account: AccountId) -> Result<Vec<String>> {
        self.collect_files(account).map(|(_, _, files)| files)
    }

    /// Step 1: collect the account's files and write the plan.
    fn prepare_delete(&self, account: AccountId, now: u64) -> Result<Prepared> {
        let (name, avatar, files) = self.collect_files(account)?;
        let manifest = Manifest {
            schema_version: MANIFEST_VERSION,
            kind: MANIFEST_KIND.to_owned(),
            account_id: account.0,
            display_name: name,
            avatar_id: avatar,
            deleted_at: now,
            deleted_at_utc: utc_text(now),
            files,
            restore: None,
        };
        let base = format!("{}-{}", utc_stamp(now), account.0);
        let trash = self.trash_dir();
        let dir = trash.join(format!("{base}{PENDING}"));
        if trash.join(&base).exists() {
            return Err(PersistError::corrupt(&trash.join(&base), "a trash entry of this name exists already"));
        }
        fsio::ensure_dir(&trash)?;
        // An earlier attempt at this very delete that never committed (the account is still here, so it did not).
        if dir.exists() {
            fs::remove_dir_all(&dir).map_err(|e| PersistError::io("remove abandoned trash entry", &dir, e))?;
        }
        fsio::ensure_dir(&dir)?;
        write_manifest(&dir, &manifest)?;
        Ok(Prepared { base, dir, manifest })
    }

    /// Step 2, the commit: the account leaves the index (the entry an orphan's release writes).
    fn commit_delete(&self, account: AccountId) -> Result<()> {
        let mut log = self.log();
        log.append(&Op::Release { account: account.0 })?;
        self.index_mut().remove_account(account.0);
        Ok(())
    }

    /// Step 3: move the files into the entry and seal it. A failure here does not undo the delete: it is reported
    /// and the next open finishes it.
    fn finish_delete(&self, prepared: Prepared) -> Deleted {
        let Prepared { base, dir, manifest } = prepared;
        let moves = move_into_entry(&self.root, &dir, &manifest);
        let trash = self.trash_dir();
        let mut unfinished = moves.failed;
        let mut entry = format!("{base}{PENDING}");
        if unfinished.is_empty() {
            match rename_entry(&trash, &dir, &trash.join(&base)) {
                Ok(()) => entry = base,
                Err(e) => unfinished.push(e.to_string()),
            }
        }
        if !unfinished.is_empty() {
            tracing::warn!(account = manifest.account_id, ?unfinished, "a deleted account's records were not all moved into the trash; the next open of the store finishes it");
        }
        Deleted {
            account_id: AccountId(manifest.account_id),
            display_name: manifest.display_name,
            entry,
            files: manifest.files,
            unfinished,
        }
    }

    /// The trash, oldest first (the names start with the time of the delete). Reads only: no lock is taken.
    pub fn trash_entries(&self) -> Result<Vec<TrashEntry>> {
        let trash = self.trash_dir();
        let mut out = Vec::new();
        for name in fsio::list_subdirs(&trash)? {
            let Some(parsed) = parse_name(&name) else { continue };
            let entry = match read_manifest(&trash.join(&name)) {
                Ok(manifest) => TrashEntry {
                    state: parsed.state,
                    account_id: AccountId(parsed.account),
                    display_name: manifest.display_name,
                    deleted_at: manifest.deleted_at,
                    deleted_at_utc: manifest.deleted_at_utc,
                    files: manifest.files.len(),
                    problem: None,
                    name,
                },
                Err(e) => TrashEntry {
                    state: parsed.state,
                    account_id: AccountId(parsed.account),
                    display_name: String::new(),
                    deleted_at: 0,
                    deleted_at_utc: String::new(),
                    files: 0,
                    problem: Some(e.to_string()),
                    name,
                },
            };
            out.push(entry);
        }
        Ok(out)
    }

    /// Everything a restore of `entry` checks before it writes anything: the entry is a complete, restorable one
    /// ([`PersistError::NoSuchTrashEntry`]), its manifest and every record in it read cleanly
    /// ([`PersistError::Corrupt`]), the store has room for another account
    /// ([`PersistError::AccountLimitReached`]) and the character's name is still free ([`PersistError::NameTaken`]).
    /// The caller holds the writer lock (or only wants to know).
    fn check_restorable(&self, entry: &str) -> Result<Checked> {
        let parsed = parse_name(entry)
            .filter(|parsed| parsed.state == TrashState::Deleted)
            .ok_or_else(|| PersistError::NoSuchTrashEntry(entry.to_owned()))?;
        let dir = self.trash_dir().join(entry);
        if !dir.is_dir() {
            return Err(PersistError::NoSuchTrashEntry(entry.to_owned()));
        }
        let manifest = read_manifest(&dir)?;
        if manifest.account_id != parsed.account {
            return Err(PersistError::corrupt(&dir, "the manifest is for another account than the entry's name says"));
        }
        let records = load_records(&dir, &manifest)?;
        let name = records.account.display_name.clone();
        let key = name_key(&name);
        {
            let index = self.index();
            if index.len() as u64 >= self.max_accounts {
                return Err(PersistError::AccountLimitReached { max: self.max_accounts });
            }
            if key.is_empty() || index.name_owner(&key).is_some() {
                return Err(PersistError::NameTaken(name));
            }
        }
        Ok(Checked { base: parsed.base.to_owned(), dir, manifest, records, name, key })
    }

    /// Would [`Store::restore_account`] restore `entry` now? Runs every check it runs, writes nothing, and returns the
    /// entry as [`Store::trash_entries`] lists it: for a dry run.
    pub fn restore_check(&self, entry: &str) -> Result<TrashEntry> {
        let _counters = self.writer();
        let checked = self.check_restorable(entry)?;
        Ok(TrashEntry {
            name: entry.to_owned(),
            state: TrashState::Deleted,
            account_id: AccountId(checked.manifest.account_id),
            display_name: checked.name,
            deleted_at: checked.manifest.deleted_at,
            deleted_at_utc: checked.manifest.deleted_at_utc,
            files: checked.manifest.files.len(),
            problem: None,
        })
    }

    /// Bring a deleted account back, as a **new** account: see the module documentation for what that means and for
    /// the crash argument. `entry` is a name from [`Store::trash_entries`] whose state is [`TrashState::Deleted`].
    ///
    /// Everything is read and checked before anything is written: the name must still be free
    /// ([`PersistError::NameTaken`]; the character's name is not changed to make it fit), the store must have room
    /// for another account ([`PersistError::AccountLimitReached`]) and every record in the entry must read cleanly
    /// ([`PersistError::Corrupt`]). A refused restore changes nothing and the entry stays where it is.
    /// [`PersistError::NoSuchTrashEntry`] if `entry` is not there or is not restorable (a delete or a restore in
    /// flight, or one that was restored already).
    pub fn restore_account(&self, entry: &str, now: u64) -> Result<Restored> {
        let mut counters = self.writer();
        let Checked { base, dir, mut manifest, records, name, key } = self.check_restorable(entry)?;
        let trash = self.trash_dir();
        let new_account = counters.next_account_id;
        let next_account = new_account.checked_add(1).ok_or(PersistError::IdExhausted)?;
        let new_avatar = records.avatar.as_ref().map(|_| counters.next_avatar_id);
        let next_avatar = match new_avatar {
            Some(id) => id.checked_add(1).ok_or(PersistError::IdExhausted)?,
            None => counters.next_avatar_id,
        };
        let token = new_token()?;
        let hash = token_hash(&token);

        // 1. The intent, so that a crash can be undone or finished: the plan goes into the manifest, and the entry
        //    takes the `.restoring` name.
        let plan = RestorePlan { account_id: new_account, avatar_id: new_avatar, at: now };
        manifest.restore = Some(plan);
        write_manifest(&dir, &manifest)?;
        let restoring = trash.join(format!("{base}{RESTORING}{new_account}"));
        rename_entry(&trash, &dir, &restoring)?;

        // 2. The records nothing references yet (what a registration writes before its one log entry).
        let written = (|| -> Result<()> {
            let account = AccountRecord {
                schema_version: SCHEMA_VERSION,
                account_id: AccountId(new_account),
                display_name: records.account.display_name.clone(),
                token_hash: hex::encode(hash),
                created_at: records.account.created_at,
            };
            fsio::write_json(&self.account_path(new_account), &account, MAX_RECORD_BYTES)?;
            if let (Some(profile), Some(avatar)) = (&records.avatar, new_avatar) {
                let mut profile = profile.clone();
                profile.schema_version = SCHEMA_VERSION;
                profile.avatar_id = AvatarId(avatar);
                profile.account_id = AccountId(new_account);
                fsio::write_json(&self.avatar_path(avatar), &profile, MAX_RECORD_BYTES)?;
            }
            if let Some(character) = &records.character {
                let mut character = character.clone();
                character.schema_version = SCHEMA_VERSION;
                character.account_id = AccountId(new_account);
                fsio::write_json(&self.character_record_path(new_account), &character, MAX_PLAYER_CHARACTER_BYTES)?;
            }
            Ok(())
        })();
        // 3. The commit: one synced `account` entry, claimed (a restored account is nobody's orphan).
        let committed = written.and_then(|()| {
            let entry = AccountEntry { name: name.clone(), token: hash, avatar: new_avatar, created_at: records.account.created_at, claimed: true };
            let op = Op::Account {
                id: new_account,
                name: name.clone(),
                token: hex::encode(hash),
                at: records.account.created_at,
                avatar: new_avatar,
                claimed: true,
            };
            let mut log = self.log();
            log.append(&op)?;
            self.index_mut().add_account(new_account, entry, key);
            Ok(())
        });
        if let Err(e) = committed {
            // Nothing committed: undo what was written and put the entry back (if this fails, the next open does).
            remove_unreferenced_restore_records(&self.root, &plan);
            manifest.restore = None;
            let _ = write_manifest(&restoring, &manifest);
            let _ = rename_entry(&trash, &restoring, &dir);
            return Err(e);
        }
        counters.next_account_id = next_account;
        counters.next_avatar_id = next_avatar;

        // 4. The progress records (after the commit, as a registration's are), then the seal.
        let mut unfinished = write_progress(&self.root, &restoring, &manifest, new_account);
        let mut final_name = restoring.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        if unfinished.is_empty() {
            let restored = trash.join(format!("{base}{RESTORED}{new_account}"));
            match rename_entry(&trash, &restoring, &restored) {
                Ok(()) => final_name = format!("{base}{RESTORED}{new_account}"),
                Err(e) => unfinished.push(e.to_string()),
            }
        }
        if !unfinished.is_empty() {
            tracing::warn!(account = new_account, ?unfinished, "a restored account's progress was not all written; the next open of the store finishes it");
        }
        Ok(Restored {
            account_id: AccountId(new_account),
            display_name: records.account.display_name,
            token,
            avatar_id: new_avatar.map(AvatarId),
            entry: final_name,
            unfinished,
        })
    }
}

#[cfg(test)]
mod tests;
