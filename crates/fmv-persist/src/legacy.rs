//! The version 1 index (`index.json`) and the marker file that takes its place.
//!
//! Version 1 kept every name, token hash and avatar link in one JSON file that was rewritten
//! whole for each registration (see `index.rs` for why that was replaced). Opening a state
//! directory that still has one migrates it transparently:
//!
//! 1. every account's display name is read from its record and its uniqueness key is derived
//!    again with `fmv_core::name_key` (the old keys were plain lower-case and do not match the
//!    new rule); when two accounts now have the same key the lower id keeps the name, the
//!    other one keeps its account and token and is reported in the open warnings,
//! 2. the complete new `index.log` is written next to the old index and renamed into place
//!    (from here on the log is the index),
//! 3. the old `index.json` is **renamed** to `index.json.v1.bak`, so the backup is
//!    byte-for-byte the file the previous version wrote,
//! 4. a small marker is written as `index.json`. It says `schema_version: 2`, so a build that
//!    only knows version 1 refuses to open the directory (`FutureSchema`: "upgrade the server,
//!    or restore a snapshot taken before the upgrade") instead of silently using a stale index
//!    and handing out ids that the new log already used.
//!
//! A crash between the steps is repaired on the next open: a log next to a full old index
//! means step 2 finished, so the log wins and steps 3 and 4 are redone; a temp file from step 2
//! is swept and the whole migration runs again.
//!
//! Every migrated account counts as having logged in (there is no way to know), so none of
//! them is ever offered to `release_unclaimed`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use fmv_core::{name_key, AvatarAppearance};
use serde::{Deserialize, Serialize};

use crate::error::{PersistError, Result};
use crate::fsio;
use crate::index::{self, parse_token_hash, AccountEntry, Index, Replayed, INDEX_LOG_FILE};
use crate::model::{AccountRecord, INDEX_LOG_VERSION, MAX_INDEX_BYTES, MAX_RECORD_BYTES};
use crate::store::ACCOUNTS_DIR;

/// The pre-v2 index, and since v2 the marker that replaces it.
pub(crate) const LEGACY_INDEX_FILE: &str = "index.json";
/// Where the pre-v2 index is kept after migration.
pub(crate) const LEGACY_BACKUP_FILE: &str = "index.json.v1.bak";

/// `index.json` of version 1. Everything a lookup needed without reading record files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct LegacyIndex {
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

impl LegacyIndex {
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

/// What `index.json` is.
pub(crate) enum LegacyState {
    Absent,
    /// The v2 marker.
    Marker,
    /// A complete version 1 index.
    Full(Box<LegacyIndex>),
}

/// Classify `<root>/index.json`. Errors for a file of a newer version, an invalid one, or a
/// damaged version 1 index.
pub(crate) fn read(root: &Path) -> Result<LegacyState> {
    let path = root.join(LEGACY_INDEX_FILE);
    let Some(bytes) = fsio::read_capped(&path, MAX_INDEX_BYTES)? else {
        return Ok(LegacyState::Absent);
    };
    let version = fsio::check_schema_version_upto(&path, &bytes, INDEX_LOG_VERSION)?;
    if version >= INDEX_LOG_VERSION {
        return Ok(LegacyState::Marker);
    }
    let index: LegacyIndex = serde_json::from_slice(&bytes)
        .map_err(|e| PersistError::corrupt(&path, format!("invalid record: {e}")))?;
    index.validate(&path)?;
    Ok(LegacyState::Full(Box::new(index)))
}

#[derive(Serialize)]
struct Marker<'a> {
    schema_version: u32,
    index: &'a str,
    note: &'a str,
}

/// Write the small `index.json` marker (atomically, owner-only like every state file).
pub(crate) fn write_marker(root: &Path) -> Result<()> {
    let marker = Marker {
        schema_version: INDEX_LOG_VERSION,
        index: INDEX_LOG_FILE,
        note: "The accounts are indexed in index.log (format 2). This file only marks the \
               state directory as format 2, so that an older server refuses to open it. The \
               index that existed before the upgrade, if any, is index.json.v1.bak; do not \
               restore it over live data.",
    };
    fsio::write_json(&root.join(LEGACY_INDEX_FILE), &marker, MAX_INDEX_BYTES)
}

/// Rename `index.json` to a backup name that is not taken (`index.json.v1.bak`, then `.1`, `.2`
/// ...), so that nothing already kept is overwritten. The caller holds the state directory lock.
fn move_aside(root: &Path) -> Result<PathBuf> {
    let from = root.join(LEGACY_INDEX_FILE);
    for n in 0..1000u32 {
        let name = if n == 0 {
            LEGACY_BACKUP_FILE.to_owned()
        } else {
            format!("{LEGACY_BACKUP_FILE}.{n}")
        };
        let to = root.join(name);
        if fs::symlink_metadata(&to).is_ok() {
            continue;
        }
        fs::rename(&from, &to)
            .map_err(|e| PersistError::io("rename to a backup name", &from, e))?;
        fsio::sync_dir(root).map_err(|e| PersistError::io("fsync directory", root, e))?;
        return Ok(to);
    }
    Err(PersistError::corrupt(
        &from,
        "too many index.json backups already exist; remove some and open again",
    ))
}

/// A version 2 index derived from a version 1 one.
struct Derived {
    index: Index,
    /// `(later account, account that holds the name)` pairs with the same key.
    collisions: Vec<(u64, u64)>,
    /// Notes for the open warnings.
    notes: Vec<String>,
}

/// Derive the version 2 index from a version 1 one, reading each account's display name from
/// its record.
fn build_index(root: &Path, old: &LegacyIndex) -> Result<Derived> {
    let legacy_path = root.join(LEGACY_INDEX_FILE);
    let mut token_of: HashMap<u64, &str> = HashMap::with_capacity(old.tokens.len());
    for (hash, &id) in &old.tokens {
        if token_of.insert(id, hash.as_str()).is_some() {
            return Err(PersistError::corrupt(
                &legacy_path,
                format!("account {id} has more than one token; this build supports one"),
            ));
        }
    }
    let by_id: BTreeMap<u64, &str> = old
        .names
        .iter()
        .map(|(key, &id)| (id, key.as_str()))
        .collect();

    let mut index = Index::default();
    let mut collisions = Vec::new();
    let mut unreadable: Vec<u64> = Vec::new();
    for (&id, &legacy_key) in &by_id {
        let record_path = root.join(ACCOUNTS_DIR).join(format!("{id}.json"));
        let record = match fsio::read_json::<AccountRecord>(&record_path, MAX_RECORD_BYTES) {
            Ok(Some(record)) if record.account_id.0 == id => Some(record),
            Ok(_) => None,
            // A record from a newer build is not something to paper over.
            Err(e @ (PersistError::FutureSchema { .. } | PersistError::InvalidSchema { .. })) => {
                return Err(e)
            }
            Err(_) => None,
        };
        if record.is_none() {
            unreadable.push(id);
        }
        let mut name = record
            .as_ref()
            .and_then(|r| AvatarAppearance::sanitize_name(&r.display_name))
            .unwrap_or_else(|| legacy_key.to_owned());
        let mut key = name_key(&name);
        if key.is_empty() {
            key = legacy_key.to_owned();
            name = legacy_key.to_owned();
        }
        let token_hex = token_of
            .get(&id)
            .copied()
            .or_else(|| record.as_ref().map(|r| r.token_hash.as_str()))
            .ok_or_else(|| {
                PersistError::corrupt(&legacy_path, format!("account {id} has no token"))
            })?;
        let token = parse_token_hash(token_hex).map_err(|reason| {
            PersistError::corrupt(&legacy_path, format!("account {id}: {reason}"))
        })?;
        let entry = AccountEntry {
            name,
            token,
            avatar: old.avatars.get(&id).copied(),
            created_at: record.map_or(0, |r| r.created_at),
            claimed: true,
        };
        if let Some(owner) = index.add_account(id, entry, key) {
            collisions.push((id, owner));
        }
    }

    let mut notes = Vec::new();
    if !unreadable.is_empty() {
        notes.push(format!(
            "{} account record(s) could not be read while migrating the index ({}); their names \
             were taken from the old index instead",
            unreadable.len(),
            describe_ids(&unreadable)
        ));
    }
    Ok(Derived {
        index,
        collisions,
        notes,
    })
}

/// "1, 2, 3, 4, 5 and 7 more".
pub(crate) fn describe_ids(ids: &[u64]) -> String {
    const LIMIT: usize = 5;
    let shown: Vec<String> = ids.iter().take(LIMIT).map(u64::to_string).collect();
    if ids.len() > LIMIT {
        format!("{} and {} more", shown.join(", "), ids.len() - LIMIT)
    } else {
        shown.join(", ")
    }
}

/// Migrate a version 1 index to `index.log` (steps 1 to 4 in the module documentation).
pub(crate) fn migrate(
    root: &Path,
    old: &LegacyIndex,
    warnings: &mut Vec<String>,
) -> Result<Replayed> {
    let Derived {
        index,
        collisions,
        notes,
    } = build_index(root, old)?;
    let log_path = root.join(INDEX_LOG_FILE);
    let (len, entries) =
        index::write_compacted(&log_path, &index, old.next_account_id, old.next_avatar_id)?;
    // From here on the log is the index; the rest only tidies up and is redone if interrupted.
    let backup = move_aside(root)?;
    write_marker(root)?;
    let message = format!(
        "migrated the version 1 account index ({} accounts) to index.log; the old index is kept \
         as {}",
        index.len(),
        backup.display()
    );
    tracing::warn!("{message}");
    warnings.push(message);
    warnings.extend(notes);
    Ok(Replayed {
        index,
        next_account_id: old.next_account_id,
        next_avatar_id: old.next_avatar_id,
        entries,
        good_len: len,
        file_len: len,
        torn: None,
        collisions,
    })
}

/// `index.log` exists: make sure `index.json` is the marker. A missing marker is written
/// quietly (the directory was created or migrated by a build that was interrupted); a full old
/// index, or a file that cannot be read, is a leftover that is moved aside with a warning. The
/// log stays authoritative either way.
pub(crate) fn settle(root: &Path, warnings: &mut Vec<String>) -> Result<()> {
    match read(root) {
        Ok(LegacyState::Marker) => Ok(()),
        Ok(LegacyState::Absent) => write_marker(root),
        Ok(LegacyState::Full(_)) => leftover(
            root,
            warnings,
            "a full version 1 index.json was found next to index.log (an interrupted migration, \
             or an old file restored by hand)",
        ),
        // A marker of a newer build is not ours to overwrite.
        Err(e @ (PersistError::FutureSchema { .. } | PersistError::InvalidSchema { .. })) => Err(e),
        Err(_) => leftover(
            root,
            warnings,
            "index.json next to index.log could not be read",
        ),
    }
}

fn leftover(root: &Path, warnings: &mut Vec<String>, what: &str) -> Result<()> {
    let backup = move_aside(root)?;
    write_marker(root)?;
    let message = format!(
        "{what}; index.log is the index, and the file was moved to {}",
        backup.display()
    );
    tracing::warn!("{message}");
    warnings.push(message);
    Ok(())
}
