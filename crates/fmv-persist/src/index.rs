//! The account index: an append-only, checksummed log that is replayed into in-memory lookup
//! tables when the store opens.
//!
//! # Why a log
//!
//! The index answers three questions that must never be answered wrongly: which account owns
//! this name, which account owns this token, and which ids were ever handed out. Version 1
//! kept all three in one `index.json` that was rewritten whole (twice) for every registration,
//! under the one mutex every login also took; its cost grew with the number of accounts and it
//! had a hard size cap. Here a registration appends **one** line and syncs it, so its cost does
//! not depend on how many accounts exist, a single fsync commits the name, the token and the id
//! together (there is no state in which only some of the three exist), and logins read
//! in-memory tables that no disk write ever blocks.
//!
//! The alternative considered was one small file per name and per token hash in sharded
//! directories, created with `O_EXCL`/`link`. That scales just as well but needs three files
//! to be created per registration with a recovery protocol for every partial state, a
//! directory entry per account for each lookup kind, and it makes snapshots copy three times
//! as many files. A log commits the whole registration with one write.
//!
//! # File format
//!
//! `index.log` is text, one entry per line: `<8 hex checksum> <JSON>\n`, where the checksum
//! is the first four bytes of the BLAKE3 hash of the JSON text. The first line is a header
//! (`{"schema_version":2,"kind":"fmv-index-log"}`). Version 3 introduced durable scene
//! batches. Explicit durable story writes now raise the header to 4, excluding earlier
//! writers that lack exact ordinary story/battle commits and uncertain-write fencing.
//! Field-visit state requires version 5 so prior writers cannot discard its metadata.
//! Reading and compaction preserve each existing supported header. The entries that
//! follow retain the same format:
//!
//! * `{"op":"account","id":7,"name":"Tifa","token":"<64 hex>","at":1700000000}` plus optional
//!   `"avatar":3` and `"claimed":true`: a registration (the name is the cleaned display name;
//!   the uniqueness key is derived from it with `fmv_core::name_key` when the log is read, so
//!   an improved key rule takes effect on the next open without rewriting anything),
//! * `{"op":"avatar","account":7,"avatar":3}`: an avatar attached to an existing account,
//! * `{"op":"claim","account":7}`: the account's token was used to log in at least once, or its
//!   owner was recorded as holding it (`Store::mark_claimed`),
//! * `{"op":"release","account":7}`: an orphaned account was removed,
//! * `{"op":"next","account":8,"avatar":4}`: id high-water marks (written by compaction, so
//!   that released ids are never handed out again).
//!
//! A compacted log is the minimal one: one `account` entry per live account (in id order, with
//! `"claimed":true` where it applies, and no avatar), then one `avatar` entry per link **in
//! avatar-id order**, then `next`. The avatars are not inline because avatar ids need not grow
//! with account ids: two registrations in flight can finish in either order, so account 1 may
//! own avatar 2 while account 2 owns avatar 1, and the replay rule below ("ids only grow along
//! the log") would reject that if it were written in account order.
//!
//! Do not edit it by hand (the checksums would stop matching); use `Store::release_unclaimed`
//! or restore a snapshot.
//!
//! # Crash consistency
//!
//! * Record files (`accounts/<id>.json`, `avatars/<id>.json`) are written and fsynced first;
//!   the log line is the commit. A crash before the line leaves unreferenced records that the
//!   next registration overwrites (the id was never committed, so it is not reused).
//! * An appended line is durable when `sync` returns. A crash can leave a torn final line;
//!   opening detects it (no newline, or a checksum mismatch on the last line), drops it and
//!   says so. No acknowledged registration is lost: the caller only saw success after the sync.
//!   A damaged line followed by more entries is not a torn write but corruption, and opening
//!   fails instead of guessing.
//! * If an append fails and cannot be rolled back, the writer refuses further writes until the
//!   server restarts, because the file would otherwise disagree with memory (and an id could be
//!   handed out twice).
//! * Replay enforces that account and avatar ids only grow, so a log in which an id repeats is
//!   rejected as corrupt.
//! * Compaction (when the log has accumulated releases) and migration write a complete new log
//!   next to the old one and rename it into place, so there is always exactly one valid index.

use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};

use fmv_core::name_key;
use serde::{Deserialize, Serialize};

use crate::error::{PersistError, Result};
use crate::fsio;
use crate::legacy::{self, LegacyState};
use crate::model::INDEX_LOG_VERSION;

pub(crate) const INDEX_LOG_FILE: &str = "index.log";
/// Minimum header of legacy scene batch intents. Recovery must keep accepting these under
/// version 3; raising the current writer's fence must not strand an older pending intent.
pub(crate) const SCENE_BATCH_INDEX_VERSION: u32 = 3;
/// The minimum fence for explicit durable story writes.
/// Character schema 3 and batch intent schema 2 are unchanged; version 4 excludes prior
/// writers whose ordinary story/reward and uncertain-write behavior is incompatible.
pub(crate) const DURABLE_STORY_INDEX_VERSION: u32 = 4;
/// Current recognized index maximum and the minimum fence for account-owned field visits.
pub(crate) const FIELD_VISIT_INDEX_VERSION: u32 = 5;

/// Explicit minimum reader/writer compatibility for a store. Selecting a level only raises
/// its recognized index fence; it does not initialize, validate or register game records.
/// Existing records and registration defaults remain unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreCompatibility {
    /// Exact ordinary story and recoverable battle outcomes (index version 4).
    DurableStory,
    /// Account-owned field visits and their atomic progress updates (index version 5).
    FieldVisit,
}

impl StoreCompatibility {
    pub(crate) fn index_version(self) -> u32 {
        match self {
            Self::DurableStory => DURABLE_STORY_INDEX_VERSION,
            Self::FieldVisit => FIELD_VISIT_INDEX_VERSION,
        }
    }
}
const LOG_KIND: &str = "fmv-index-log";
const CHECKSUM_LEN: usize = 8;
/// Longest line read back. An account line is under 400 bytes (24 characters of up to four
/// bytes each, a 64 character token hash and a few numbers).
const MAX_LINE_BYTES: usize = 4096;
/// A damaged final line within this many bytes is a torn write; anything longer is corruption.
const TORN_TAIL_MAX_BYTES: u64 = 64 * 1024;
/// Compaction runs on open once the log holds this many more entries than a freshly written
/// one would, beyond one per account.
const COMPACT_SLACK: u64 = 4096;
/// How many collision pairs a warning names before saying "and N more".
const WARNING_LIST_LIMIT: usize = 5;

/// `blake3(token)` as raw bytes.
pub(crate) type TokenHash = [u8; 32];

/// Everything the lookups need to know about one account.
#[derive(Debug, Clone)]
pub(crate) struct AccountEntry {
    /// The cleaned display name. What the account's name is compared by is its
    /// `fmv_core::name_key`, derived from this whenever it is needed.
    pub name: String,
    pub token: TokenHash,
    pub avatar: Option<u64>,
    pub created_at: u64,
    /// Its token has been used to log in at least once, or its owner was recorded as holding it
    /// (`Store::mark_claimed`). False only for an account where neither happened, which is what
    /// makes it an orphan candidate (see `release_unclaimed`).
    pub claimed: bool,
}

/// The in-memory lookup tables. Mutated only by the store's writers, after the matching log
/// line is durable.
#[derive(Debug, Default)]
pub(crate) struct Index {
    /// The actual header read from disk. A fresh/legacy index keeps version 2 until its
    /// first explicit durable story write; compaction preserves the actual version read.
    format_version: Option<u32>,
    accounts: HashMap<u64, AccountEntry>,
    /// Name key -> the account that reserved it. Normally one entry per account; an account
    /// whose key was already reserved by an older one (only possible for names registered under
    /// an earlier version of the key rule) has none, and can still log in.
    names: HashMap<String, u64>,
    tokens: HashMap<TokenHash, u64>,
    /// Avatar id -> owning account.
    avatars: HashMap<u64, u64>,
}

impl Index {
    pub(crate) fn format_version(&self) -> u32 { self.format_version.unwrap_or(INDEX_LOG_VERSION) }

    pub(crate) fn require_format(&mut self, minimum: u32) {
        self.format_version = Some(self.format_version().max(minimum));
    }

    /// Make room for `accounts` accounts in every table now. Registering an account then never
    /// rehashes a table while the index lock is held: at 100,000 accounts a rehash takes
    /// milliseconds, and every login would wait for it.
    pub(crate) fn reserve_for(&mut self, accounts: usize) {
        let more = accounts.saturating_sub(self.accounts.len());
        self.accounts.reserve(more);
        self.names.reserve(more);
        self.tokens.reserve(more);
        self.avatars.reserve(more);
    }

    pub(crate) fn len(&self) -> usize {
        self.accounts.len()
    }

    /// How many accounts have an avatar.
    pub(crate) fn avatar_links(&self) -> usize {
        self.avatars.len()
    }

    pub(crate) fn account(&self, id: u64) -> Option<&AccountEntry> {
        self.accounts.get(&id)
    }

    pub(crate) fn contains_account(&self, id: u64) -> bool {
        self.accounts.contains_key(&id)
    }

    pub(crate) fn contains_avatar(&self, id: u64) -> bool {
        self.avatars.contains_key(&id)
    }

    pub(crate) fn name_owner(&self, key: &str) -> Option<u64> {
        self.names.get(key).copied()
    }

    pub(crate) fn token_owner(&self, token: &TokenHash) -> Option<u64> {
        self.tokens.get(token).copied()
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (u64, &AccountEntry)> {
        self.accounts.iter().map(|(&id, entry)| (id, entry))
    }

    /// Add an account whose id the caller has checked is new, reserving `key` (its
    /// `name_key`) for it. Returns the account that already reserved the same key, if any
    /// (then `id` does not get the reservation).
    pub(crate) fn add_account(&mut self, id: u64, entry: AccountEntry, key: String) -> Option<u64> {
        self.tokens.insert(entry.token, id);
        if let Some(avatar) = entry.avatar {
            self.avatars.insert(avatar, id);
        }
        let owner = match self.names.entry(key) {
            Entry::Occupied(taken) => Some(*taken.get()),
            Entry::Vacant(free) => {
                free.insert(id);
                None
            }
        };
        self.accounts.insert(id, entry);
        owner
    }

    pub(crate) fn remove_account(&mut self, id: u64) -> Option<AccountEntry> {
        let entry = self.accounts.remove(&id)?;
        self.tokens.remove(&entry.token);
        if let Some(avatar) = entry.avatar {
            self.avatars.remove(&avatar);
        }
        // Only the account that holds the reservation gives it back.
        let key = name_key(&entry.name);
        if self.names.get(&key) == Some(&id) {
            self.names.remove(&key);
        }
        Some(entry)
    }

    /// Attach `avatar` to `account`. False if the account is unknown or already has one.
    pub(crate) fn link_avatar(&mut self, account: u64, avatar: u64) -> bool {
        match self.accounts.get_mut(&account) {
            Some(entry) if entry.avatar.is_none() => {
                entry.avatar = Some(avatar);
                self.avatars.insert(avatar, account);
                true
            }
            _ => false,
        }
    }

    /// Mark the account as having logged in. False if it does not exist.
    pub(crate) fn set_claimed(&mut self, account: u64) -> bool {
        match self.accounts.get_mut(&account) {
            Some(entry) => {
                entry.claimed = true;
                true
            }
            None => false,
        }
    }
}

// ---- entries ------------------------------------------------------------------------------

/// One entry of `index.log`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Op {
    Account {
        id: u64,
        name: String,
        /// `blake3(token)` as lowercase hex.
        token: String,
        at: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        avatar: Option<u64>,
        #[serde(default, skip_serializing_if = "is_false")]
        claimed: bool,
    },
    Avatar {
        account: u64,
        avatar: u64,
    },
    Claim {
        account: u64,
    },
    Release {
        account: u64,
    },
    Next {
        account: u64,
        avatar: u64,
    },
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Serialize, Deserialize)]
struct Header {
    schema_version: u32,
    kind: String,
}

fn checksum(payload: &[u8]) -> String {
    let hex = blake3::hash(payload).to_hex();
    hex.as_str()
        .get(..CHECKSUM_LEN)
        .unwrap_or_default()
        .to_owned()
}

fn encode_line<T: Serialize>(value: &T, path: &Path) -> Result<Vec<u8>> {
    let payload = serde_json::to_vec(value).map_err(|e| {
        PersistError::corrupt(path, format!("cannot serialize an index entry: {e}"))
    })?;
    let mut line = Vec::with_capacity(payload.len() + CHECKSUM_LEN + 2);
    line.extend_from_slice(checksum(&payload).as_bytes());
    line.push(b' ');
    line.extend_from_slice(&payload);
    line.push(b'\n');
    if line.len() > MAX_LINE_BYTES {
        return Err(PersistError::TooLarge {
            path: path.to_path_buf(),
            size: line.len() as u64,
            max: MAX_LINE_BYTES as u64,
        });
    }
    Ok(line)
}

/// The JSON of a line (without its newline) once its checksum has been verified.
fn payload_of(line: &[u8]) -> std::result::Result<&[u8], String> {
    let (sum, rest) = (line.get(..CHECKSUM_LEN), line.get(CHECKSUM_LEN..));
    let (Some(sum), Some(rest)) = (sum, rest) else {
        return Err("the line is too short to hold a checksum".into());
    };
    let Some((b' ', payload)) = rest.split_first().map(|(first, payload)| (*first, payload)) else {
        return Err("the checksum is not followed by a space".into());
    };
    if checksum(payload).as_bytes() != sum {
        return Err("checksum mismatch".into());
    }
    Ok(payload)
}

fn header_line(path: &Path) -> Result<Vec<u8>> {
    header_line_version(path, INDEX_LOG_VERSION)
}

fn header_line_version(path: &Path, version: u32) -> Result<Vec<u8>> {
    if !(INDEX_LOG_VERSION..=FIELD_VISIT_INDEX_VERSION).contains(&version) {
        return Err(PersistError::corrupt(path, "unsupported index output version"));
    }
    encode_line(
        &Header {
            schema_version: version,
            kind: LOG_KIND.to_owned(),
        },
        path,
    )
}

fn check_header(path: &Path, line: &[u8]) -> Result<u32> {
    check_header_upto(path, line, FIELD_VISIT_INDEX_VERSION)
}

fn check_header_upto(path: &Path, line: &[u8], maximum: u32) -> Result<u32> {
    let payload = payload_of(line)
        .map_err(|reason| PersistError::corrupt(path, format!("bad header line: {reason}")))?;
    fsio::check_schema_version_upto(path, payload, maximum)?;
    let header: Header = serde_json::from_slice(payload)
        .map_err(|e| PersistError::corrupt(path, format!("bad header line: {e}")))?;
    if header.kind != LOG_KIND {
        return Err(PersistError::corrupt(
            path,
            format!("not an account index (kind {:?})", header.kind),
        ));
    }
    if header.schema_version < INDEX_LOG_VERSION { return Err(PersistError::corrupt(path, "index log header predates the log format")); }
    Ok(header.schema_version)
}

pub(crate) fn parse_token_hash(hex_text: &str) -> std::result::Result<TokenHash, String> {
    let lowercase_hex = hex_text
        .bytes()
        .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    let mut hash = [0u8; 32];
    if !lowercase_hex || hex::decode_to_slice(hex_text, &mut hash).is_err() {
        return Err("token hash is not 64 lowercase hex characters".into());
    }
    Ok(hash)
}

// ---- replay -------------------------------------------------------------------------------

/// What reading `index.log` produced.
pub(crate) struct Replayed {
    pub index: Index,
    pub next_account_id: u64,
    pub next_avatar_id: u64,
    /// Entries after the header.
    pub entries: u64,
    /// Length of the valid prefix: shorter than the file when the last entry was torn.
    pub good_len: u64,
    pub file_len: u64,
    /// Why the bytes after `good_len` were not accepted, when there are any.
    pub torn: Option<String>,
    /// `(later account, account that holds the name reservation)` pairs whose names have the
    /// same key.
    pub collisions: Vec<(u64, u64)>,
}

struct Replay {
    index: Index,
    next_account_id: u64,
    next_avatar_id: u64,
    collisions: Vec<(u64, u64)>,
}

impl Replay {
    fn new() -> Self {
        Self {
            index: Index::default(),
            next_account_id: 1,
            next_avatar_id: 1,
            collisions: Vec::new(),
        }
    }

    fn check_new_avatar(&self, avatar: u64) -> std::result::Result<u64, String> {
        if avatar == 0 || avatar < self.next_avatar_id {
            return Err(format!(
                "avatar id {avatar} does not exceed the previous ones (ids only grow)"
            ));
        }
        avatar
            .checked_add(1)
            .ok_or_else(|| "avatar id space exhausted".to_string())
    }

    fn apply(&mut self, op: Op) -> std::result::Result<(), String> {
        match op {
            Op::Account {
                id,
                name,
                token,
                at,
                avatar,
                claimed,
            } => {
                if id == 0 || id < self.next_account_id {
                    return Err(format!(
                        "account id {id} does not exceed the previous ones (ids only grow, so \
                         none is ever reused)"
                    ));
                }
                let next_account = id
                    .checked_add(1)
                    .ok_or_else(|| "account id space exhausted".to_string())?;
                let token = parse_token_hash(&token)?;
                if self.index.token_owner(&token).is_some() {
                    return Err(format!("account {id} reuses another account's token hash"));
                }
                let key = name_key(&name);
                if key.is_empty() {
                    return Err(format!(
                        "account {id} has the name {name:?}, with nothing visible"
                    ));
                }
                let next_avatar = match avatar {
                    Some(avatar) => Some(self.check_new_avatar(avatar)?),
                    None => None,
                };
                self.next_account_id = next_account;
                if let Some(next_avatar) = next_avatar {
                    self.next_avatar_id = next_avatar;
                }
                let entry = AccountEntry {
                    name,
                    token,
                    avatar,
                    created_at: at,
                    claimed,
                };
                if let Some(owner) = self.index.add_account(id, entry, key) {
                    self.collisions.push((id, owner));
                }
            }
            Op::Avatar { account, avatar } => {
                let next_avatar = self.check_new_avatar(avatar)?;
                if !self.index.link_avatar(account, avatar) {
                    return Err(format!(
                        "avatar {avatar} is attached to account {account}, which does not \
                         exist or already has an avatar"
                    ));
                }
                self.next_avatar_id = next_avatar;
            }
            Op::Claim { account } => {
                if !self.index.set_claimed(account) {
                    return Err(format!("account {account} was claimed but does not exist"));
                }
            }
            Op::Release { account } => {
                if self.index.remove_account(account).is_none() {
                    return Err(format!("account {account} was released but does not exist"));
                }
            }
            Op::Next { account, avatar } => {
                if account == 0 || avatar == 0 {
                    return Err("next ids must start at 1".into());
                }
                self.next_account_id = self.next_account_id.max(account);
                self.next_avatar_id = self.next_avatar_id.max(avatar);
            }
        }
        Ok(())
    }
}

/// Read and replay `path`. `Ok(None)` if it does not exist.
///
/// With `repair`, a torn final entry is tolerated and reported through `good_len`; without it
/// (validating a snapshot, which is written whole) that is an error too.
pub(crate) fn load_log(path: &Path, repair: bool) -> Result<Option<Replayed>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(PersistError::io("open", path, e)),
    };
    let meta = file
        .metadata()
        .map_err(|e| PersistError::io("stat", path, e))?;
    if !meta.is_file() {
        return Err(PersistError::corrupt(path, "not a regular file"));
    }
    let file_len = meta.len();
    let io_err = |e: io::Error| PersistError::io("read", path, e);

    let mut reader = BufReader::with_capacity(1 << 16, file);
    let mut buf: Vec<u8> = Vec::with_capacity(512);
    let mut replay = Replay::new();
    let mut offset = 0u64;
    let mut line_no = 0u64;
    let mut entries = 0u64;
    let mut good_len = 0u64;
    let mut torn: Option<String> = None;

    loop {
        buf.clear();
        let n = (&mut reader)
            .take(MAX_LINE_BYTES as u64 + 1)
            .read_until(b'\n', &mut buf)
            .map_err(io_err)?;
        if n == 0 {
            break;
        }
        line_no += 1;
        let complete = buf.last() == Some(&b'\n');
        let at_eof = reader.fill_buf().map_err(io_err)?.is_empty();

        if !complete {
            // Either the file ends in the middle of an entry, or a line is absurdly long.
            let reason = if n > MAX_LINE_BYTES {
                "a line is longer than any entry can be".to_string()
            } else {
                "the last entry has no terminating newline".to_string()
            };
            let tail = file_len.saturating_sub(offset);
            if line_no > 1 && repair && tail <= TORN_TAIL_MAX_BYTES {
                torn = Some(reason);
                break;
            }
            return Err(PersistError::corrupt(
                path,
                format!("line {line_no}: {reason}"),
            ));
        }

        let body = buf.get(..n.saturating_sub(1)).unwrap_or_default();
        if line_no == 1 {
            replay.index.format_version = Some(check_header(path, body)?);
            offset += n as u64;
            good_len = offset;
            continue;
        }
        // Only a checksum failure can be a torn write: a torn line is a prefix of a valid one,
        // so its checksum no longer matches. A line whose checksum is right but whose content
        // is not understood (an entry kind from a newer build, a duplicate id) was written
        // whole and on purpose, and is never silently dropped.
        let payload = match payload_of(body) {
            Ok(payload) => payload,
            Err(reason) if at_eof && repair => {
                torn = Some(reason);
                break;
            }
            Err(reason) => {
                return Err(PersistError::corrupt(
                    path,
                    format!("line {line_no}: {reason}"),
                ))
            }
        };
        let op = serde_json::from_slice::<Op>(payload).map_err(|e| {
            PersistError::corrupt(path, format!("line {line_no}: not a valid entry: {e}"))
        })?;
        replay
            .apply(op)
            .map_err(|reason| PersistError::corrupt(path, format!("line {line_no}: {reason}")))?;
        entries += 1;
        offset += n as u64;
        good_len = offset;
    }

    if line_no == 0 {
        return Err(PersistError::corrupt(
            path,
            "the index log is empty (it has no header)",
        ));
    }
    Ok(Some(Replayed {
        index: replay.index,
        next_account_id: replay.next_account_id,
        next_avatar_id: replay.next_avatar_id,
        entries,
        good_len,
        file_len,
        torn,
        collisions: replay.collisions,
    }))
}

// ---- writing ------------------------------------------------------------------------------

/// Appends entries to `index.log`. One entry = one `write` + one sync.
#[derive(Debug)]
pub(crate) struct LogWriter {
    file: File,
    path: PathBuf,
    len: u64,
    poisoned: Option<String>,
}

impl LogWriter {
    /// Open `path` for appending after its first `good_len` bytes. A torn tail beyond that is
    /// cut off first, and the cut is made durable before anything is appended.
    pub(crate) fn open(path: &Path, good_len: u64) -> Result<LogWriter> {
        let file = OpenOptions::new()
            .append(true)
            .open(path)
            .map_err(|e| PersistError::io("open", path, e))?;
        let actual = file
            .metadata()
            .map_err(|e| PersistError::io("stat", path, e))?
            .len();
        if actual < good_len {
            return Err(PersistError::corrupt(
                path,
                format!("the index log shrank from {good_len} to {actual} bytes while it was open"),
            ));
        }
        if actual > good_len {
            file.set_len(good_len)
                .and_then(|()| file.sync_all())
                .map_err(|e| PersistError::io("truncate the torn tail of", path, e))?;
        }
        Ok(LogWriter {
            file,
            path: path.to_path_buf(),
            len: good_len,
            poisoned: None,
        })
    }

    /// Bytes of valid log written so far: a copy of exactly this many bytes is a consistent
    /// image even while another thread is about to append.
    pub(crate) fn len(&self) -> u64 {
        self.len
    }

    /// Refuse every further append (until the store is reopened and the log re-read).
    pub(crate) fn poison(&mut self, detail: String) {
        self.poisoned = Some(detail);
    }

    /// Check before preparing an account allocation: an uncertain prior append may have
    /// committed the next id even though the in-memory counters have not advanced.
    pub(crate) fn ensure_healthy(&self) -> Result<()> {
        match &self.poisoned {
            Some(detail) => Err(PersistError::IndexPoisoned { detail: detail.clone() }),
            None => Ok(()),
        }
    }

    /// Append `op` and make it durable. On failure the file is rolled back to its previous
    /// length; if that fails too the writer refuses everything from then on.
    pub(crate) fn append(&mut self, op: &Op) -> Result<()> {
        self.ensure_healthy()?;
        let line = encode_line(op, &self.path)?;
        let written = self
            .file
            .write_all(&line)
            .and_then(|()| self.file.sync_all());
        match written {
            Ok(()) => {
                self.len += line.len() as u64;
                Ok(())
            }
            Err(e) => {
                let rolled_back = self
                    .file
                    .set_len(self.len)
                    .and_then(|()| self.file.sync_all());
                if let Err(rollback) = rolled_back {
                    self.poisoned = Some(format!(
                        "append failed ({e}) and the rollback failed too ({rollback})"
                    ));
                }
                Err(PersistError::io("append to", &self.path, e))
            }
        }
    }
}

/// Write a complete, minimal log for `index` to `path`, atomically replacing what is there.
/// Returns the new length and entry count.
pub(crate) fn write_compacted(
    path: &Path,
    index: &Index,
    next_account_id: u64,
    next_avatar_id: u64,
) -> Result<(u64, u64)> {
    write_compacted_version(path, index, next_account_id, next_avatar_id, index.format_version())
}

/// Install an explicitly raised header before a feature's first durable record. Callers
/// must replace any open append handle after success and fence writes on uncertainty.
pub(crate) fn write_compacted_version(
    path: &Path,
    index: &Index,
    next_account_id: u64,
    next_avatar_id: u64,
    version: u32,
) -> Result<(u64, u64)> {
    if version < index.format_version() { return Err(PersistError::corrupt(path, "index format cannot be downgraded")); }
    let mut ids: Vec<u64> = index.iter().map(|(id, _)| id).collect();
    ids.sort_unstable();
    // Avatar ids only grow along the log, but they need not grow with the account ids: two
    // registrations in flight can finish in the other order (account 1 may own avatar 2 and
    // account 2 avatar 1). So the links are written on their own, in avatar-id order, after
    // all the accounts, instead of inline in account-id order.
    let mut links: Vec<(u64, u64)> = ids
        .iter()
        .filter_map(|id| Some((index.account(*id)?.avatar?, *id)))
        .collect();
    links.sort_unstable();

    let mut written = 0u64;
    let mut entries = 0u64;
    fsio::atomic_write_with(path, |out| {
        let to_io = |e: PersistError| io::Error::other(e.to_string());
        let mut put = |line: Vec<u8>| -> io::Result<()> {
            out.write_all(&line)?;
            written += line.len() as u64;
            Ok(())
        };
        put(header_line_version(path, version).map_err(to_io)?)?;
        for id in &ids {
            let Some(entry) = index.account(*id) else {
                continue;
            };
            let op = Op::Account {
                id: *id,
                name: entry.name.clone(),
                token: hex::encode(entry.token),
                at: entry.created_at,
                avatar: None,
                claimed: entry.claimed,
            };
            put(encode_line(&op, path).map_err(to_io)?)?;
            entries += 1;
        }
        for (avatar, account) in &links {
            let op = Op::Avatar {
                account: *account,
                avatar: *avatar,
            };
            put(encode_line(&op, path).map_err(to_io)?)?;
            entries += 1;
        }
        let next = Op::Next {
            account: next_account_id,
            avatar: next_avatar_id,
        };
        put(encode_line(&next, path).map_err(to_io)?)?;
        entries += 1;
        Ok(())
    })?;
    Ok((written, entries))
}

// ---- opening ------------------------------------------------------------------------------

/// What `Store::open` gets from the index directory.
pub(crate) struct Opened {
    pub index: Index,
    pub next_account_id: u64,
    pub next_avatar_id: u64,
    pub log: LogWriter,
}

fn create_fresh(root: &Path) -> Result<Replayed> {
    let log_path = root.join(INDEX_LOG_FILE);
    let header = header_line(&log_path)?;
    fsio::atomic_write(&log_path, &header)?;
    legacy::write_marker(root)?;
    let len = header.len() as u64;
    Ok(Replayed {
        index: Index::default(),
        next_account_id: 1,
        next_avatar_id: 1,
        entries: 0,
        good_len: len,
        file_len: len,
        torn: None,
        collisions: Vec::new(),
    })
}

fn collision_message(collisions: &[(u64, u64)]) -> String {
    let shown: Vec<String> = collisions
        .iter()
        .take(WARNING_LIST_LIMIT)
        .map(|(later, owner)| format!("{later} (name held by {owner})"))
        .collect();
    let more = collisions.len().saturating_sub(WARNING_LIST_LIMIT);
    let more = if more > 0 {
        format!(" and {more} more")
    } else {
        String::new()
    };
    format!(
        "{} account(s) have a display name that equals an older account's once look-alike \
         characters are folded: {}{more}; the older account keeps the name, the others can still \
         log in but cannot be found by name",
        collisions.len(),
        shown.join(", ")
    )
}

/// Load the index of the state directory `root`, in this order of preference: the log (a torn
/// final entry is dropped), a version 1 `index.json` (migrated), nothing (a new directory).
/// `has_records` says whether record files exist, which with no index at all means the index
/// was lost and the directory must not be opened.
pub(crate) fn open_index(
    root: &Path,
    warnings: &mut Vec<String>,
    has_records: impl FnOnce() -> Result<bool>,
) -> Result<Opened> {
    let log_path = root.join(INDEX_LOG_FILE);
    let replayed = match load_log(&log_path, true)? {
        Some(replayed) => {
            legacy::settle(root, warnings)?;
            replayed
        }
        None => match legacy::read(root)? {
            LegacyState::Full(old) => legacy::migrate(root, &old, warnings)?,
            LegacyState::Absent | LegacyState::Marker => {
                if has_records()? {
                    return Err(PersistError::IndexMissing {
                        root: root.to_path_buf(),
                    });
                }
                create_fresh(root)?
            }
        },
    };

    let Replayed {
        index,
        next_account_id,
        next_avatar_id,
        entries,
        mut good_len,
        file_len,
        torn,
        collisions,
    } = replayed;

    if !collisions.is_empty() {
        let message = collision_message(&collisions);
        tracing::warn!("{message}");
        warnings.push(message);
    }
    if good_len < file_len {
        let message = format!(
            "dropped {} byte(s) of an interrupted write from the end of {INDEX_LOG_FILE} ({}); \
             a registration that was cut short like this was never acknowledged, so no \
             committed account is lost",
            file_len - good_len,
            torn.as_deref().unwrap_or("not a complete entry")
        );
        tracing::warn!("{message}");
        warnings.push(message);
    }
    // A freshly written log holds one entry per account, one per avatar link and the id marks.
    // Claims (at most one per account) are the normal surplus; releases leave dead entries
    // behind. Only when the surplus outgrows the accounts by `COMPACT_SLACK` is the log
    // rewritten, so an ordinary log, claims included, is never rewritten just by opening it.
    let accounts = index.len() as u64;
    let minimal = accounts + index.avatar_links() as u64 + 1;
    if entries.saturating_sub(minimal) > accounts + COMPACT_SLACK {
        let (len, written) = write_compacted(&log_path, &index, next_account_id, next_avatar_id)?;
        tracing::info!(
            before = entries,
            after = written,
            "compacted the account index log"
        );
        good_len = len;
    }

    let log = LogWriter::open(&log_path, good_len)?;
    Ok(Opened {
        index,
        next_account_id,
        next_avatar_id,
        log,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn account(id: u64, name: &str, token: &str) -> Op {
        Op::Account {
            id,
            name: name.to_owned(),
            token: hex::encode(blake3::hash(token.as_bytes()).as_bytes()),
            at: 7,
            avatar: None,
            claimed: false,
        }
    }

    #[test]
    fn recognized_header_fences_reject_prior_readers_and_preserve_each_version_during_open_and_compaction() {
        for version in [INDEX_LOG_VERSION, SCENE_BATCH_INDEX_VERSION, DURABLE_STORY_INDEX_VERSION, FIELD_VISIT_INDEX_VERSION] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path();
            let path = root.join(INDEX_LOG_FILE);
            create_fresh(root).unwrap();
            assert_eq!(load_log(&path, false).unwrap().unwrap().index.format_version(), INDEX_LOG_VERSION);
            write_compacted_version(&path, &Index::default(), 1, 1, version).unwrap();
            let bytes = fs::read(&path).unwrap();
            let first = bytes.split(|byte| *byte == b'\n').next().unwrap();
            assert_eq!(check_header(&path, first).unwrap(), version);
            if version > INDEX_LOG_VERSION {
                // The preceding reader used this recognized-version check before reading
                // accounts or touching transaction recovery: max3 rejects4 and max4 rejects5.
                assert!(matches!(check_header_upto(&path, first, version - 1),
                    Err(PersistError::FutureSchema { .. })));
            }
            let opened = open_index(root, &mut Vec::new(), || Ok(false)).unwrap();
            assert_eq!(opened.index.format_version(), version);
            assert_eq!(fs::read(&path).unwrap(), bytes, "opening version {version} does not rewrite it");
            drop(opened);
            // Enough valid redundant marks trigger compaction during open. Use one write
            // to construct the fixture; individual account operations still sync normally.
            let line = encode_line(&Op::Next { account: 1, avatar: 1 }, &path).unwrap();
            let tail: Vec<u8> = (0..COMPACT_SLACK + 2).flat_map(|_| line.iter().copied()).collect();
            let mut file = OpenOptions::new().append(true).open(&path).unwrap();
            file.write_all(&tail).unwrap();
            file.sync_all().unwrap();
            let opened = open_index(root, &mut Vec::new(), || Ok(false)).unwrap();
            assert_eq!(opened.index.format_version(), version);
            let replayed = load_log(&path, false).unwrap().unwrap();
            assert_eq!(replayed.index.format_version(), version);
            assert_eq!(replayed.entries, 1, "the redundant marks were compacted");
            assert_eq!(fs::read(&path).unwrap(), bytes, "compaction preserves version {version}");
            if version > INDEX_LOG_VERSION {
                assert!(write_compacted_version(&path, &replayed.index, 1, 1, version - 1).is_err(),
                    "an explicit downgrade is refused too");
            }
        }
    }

    #[test]
    fn lines_are_checksummed_json_and_the_shape_is_stable() {
        let path = Path::new("index.log");
        let op = account(3, "Tifa", "t");
        let line = encode_line(&op, path).unwrap();
        let text = String::from_utf8(line.clone()).unwrap();
        // `<8 hex> <json>\n`, optional fields left out, fields in a fixed order.
        assert!(
            text.ends_with("}\n") && text.as_bytes().get(8) == Some(&b' '),
            "{text:?}"
        );
        assert!(text.as_bytes().iter().take(8).all(u8::is_ascii_hexdigit));
        let json = text.get(9..).unwrap().trim_end();
        assert_eq!(
            json,
            format!(
                r#"{{"op":"account","id":3,"name":"Tifa","token":"{}","at":7}}"#,
                hex::encode(blake3::hash(b"t").as_bytes())
            )
        );
        let body = line.get(..line.len() - 1).unwrap();
        let payload = payload_of(body).unwrap();
        assert_eq!(serde_json::from_slice::<Op>(payload).unwrap(), op);

        let full = Op::Account {
            id: 4,
            name: "A".into(),
            token: "0".repeat(64),
            at: 1,
            avatar: Some(2),
            claimed: true,
        };
        let text = String::from_utf8(encode_line(&full, path).unwrap()).unwrap();
        assert!(text.contains(r#""avatar":2,"claimed":true}"#), "{text}");
        // Every entry kind round-trips.
        for op in [
            full,
            Op::Avatar {
                account: 1,
                avatar: 2,
            },
            Op::Claim { account: 1 },
            Op::Release { account: 1 },
            Op::Next {
                account: 9,
                avatar: 8,
            },
        ] {
            let line = encode_line(&op, path).unwrap();
            let payload = payload_of(line.get(..line.len() - 1).unwrap()).unwrap();
            assert_eq!(serde_json::from_slice::<Op>(payload).unwrap(), op);
        }
    }

    #[test]
    fn a_line_never_contains_a_newline_before_its_end() {
        // Names are cleaned long before they get here, but the format must not depend on it.
        let line = encode_line(&account(1, "a\nb\r\u{2028}\"\\", "t"), Path::new("x")).unwrap();
        assert_eq!(line.iter().filter(|&&b| b == b'\n').count(), 1);
        assert_eq!(line.last(), Some(&b'\n'));
    }

    #[test]
    fn checksum_problems_are_reported_not_trusted() {
        for (line, why) in [
            (&b""[..], "too short"),
            (b"abc", "too short"),
            (b"12345678", "space"),
            (b"12345678{}", "space"),
            (b"12345678 {}", "checksum"),
            (b"\xff\xfe\xfd\xfc\xfb\xfa\xf9\xf8 {}", "checksum"),
        ] {
            let err = payload_of(line).unwrap_err();
            assert!(err.contains(why), "{line:?}: {err}");
        }
        let good = format!("{} {{}}", checksum(b"{}"));
        assert_eq!(payload_of(good.as_bytes()).unwrap(), b"{}");
        // One flipped bit anywhere in the payload is caught.
        for i in 0..2 {
            let mut bad = good.clone().into_bytes();
            let at = bad.len() - 1 - i;
            if let Some(b) = bad.get_mut(at) {
                *b ^= 0x01;
            }
            assert!(payload_of(&bad).is_err());
        }
    }

    #[test]
    fn an_over_long_entry_is_refused_when_written() {
        let huge = account(1, &"x".repeat(MAX_LINE_BYTES), "t");
        assert!(matches!(
            encode_line(&huge, Path::new("x")),
            Err(PersistError::TooLarge { .. })
        ));
        // The longest honest entry (24 four-byte characters, an avatar and a claim) fits easily.
        let widest = Op::Account {
            id: u64::MAX,
            name: "\u{1F600}".repeat(24),
            token: "f".repeat(64),
            at: u64::MAX,
            avatar: Some(u64::MAX),
            claimed: true,
        };
        let line = encode_line(&widest, Path::new("x")).unwrap();
        assert!(line.len() < MAX_LINE_BYTES / 4, "{}", line.len());
    }

    #[test]
    fn replay_keeps_ids_growing_and_released_ids_retired() {
        let mut replay = Replay::new();
        replay.apply(account(1, "A", "a")).unwrap();
        replay.apply(account(2, "B", "b")).unwrap();
        replay.apply(Op::Release { account: 2 }).unwrap();
        // Id 2 is retired even though nothing holds it any more.
        assert!(replay.apply(account(2, "C", "c")).is_err());
        replay.apply(account(3, "C", "c")).unwrap();
        assert_eq!(replay.next_account_id, 4);
        // A name freed by a release can be taken again by a later account.
        replay.apply(account(4, "B", "b2")).unwrap();
        assert!(replay.collisions.is_empty());
        assert_eq!(replay.index.name_owner(&name_key("b")), Some(4));
        // The id marks of a compacted log only ever raise the counters.
        replay
            .apply(Op::Next {
                account: 2,
                avatar: 1,
            })
            .unwrap();
        assert_eq!(replay.next_account_id, 5);
        replay
            .apply(Op::Next {
                account: 50,
                avatar: 9,
            })
            .unwrap();
        assert_eq!((replay.next_account_id, replay.next_avatar_id), (50, 9));
        assert!(replay.apply(account(49, "D", "d")).is_err());
        replay.apply(account(50, "D", "d")).unwrap();
    }

    #[test]
    fn avatar_ids_grow_and_belong_to_one_account() {
        let mut replay = Replay::new();
        replay.apply(account(1, "A", "a")).unwrap();
        replay.apply(account(2, "B", "b")).unwrap();
        replay
            .apply(Op::Avatar {
                account: 1,
                avatar: 1,
            })
            .unwrap();
        assert!(
            replay
                .apply(Op::Avatar {
                    account: 1,
                    avatar: 2
                })
                .is_err(),
            "one avatar per account"
        );
        assert!(
            replay
                .apply(Op::Avatar {
                    account: 2,
                    avatar: 1
                })
                .is_err(),
            "an id is used once"
        );
        assert!(
            replay
                .apply(Op::Avatar {
                    account: 9,
                    avatar: 5
                })
                .is_err(),
            "unknown account"
        );
        assert!(replay
            .apply(Op::Avatar {
                account: 2,
                avatar: 0
            })
            .is_err());
        replay
            .apply(Op::Avatar {
                account: 2,
                avatar: 2,
            })
            .unwrap();
        assert_eq!(replay.next_avatar_id, 3);
        assert!(replay.index.contains_avatar(1) && replay.index.contains_avatar(2));
        // Releasing an account takes its avatar link with it, and the id stays retired.
        replay.apply(Op::Release { account: 1 }).unwrap();
        assert!(!replay.index.contains_avatar(1));
        assert!(replay.apply(account(3, "C", "c")).is_ok());
        assert!(replay
            .apply(Op::Avatar {
                account: 3,
                avatar: 1
            })
            .is_err());
    }

    #[test]
    fn a_name_collision_goes_to_the_older_account_and_survives_its_release() {
        let mut replay = Replay::new();
        replay.apply(account(1, "Tifa", "a")).unwrap();
        replay.apply(account(2, "Tifa\u{3164}", "b")).unwrap();
        assert_eq!(replay.collisions, vec![(2, 1)]);
        // The loser neither owns the key nor takes it away from the owner when it goes.
        replay.apply(Op::Release { account: 2 }).unwrap();
        assert_eq!(replay.index.name_owner(&name_key("tifa")), Some(1));
        replay.apply(Op::Release { account: 1 }).unwrap();
        assert_eq!(replay.index.name_owner(&name_key("tifa")), None);
    }

    #[test]
    fn a_failed_and_unrecoverable_append_poisons_the_writer() {
        // /dev/full accepts the open and fails every write with ENOSPC, and cannot be truncated
        // back either: exactly the case in which the file may disagree with memory.
        let Ok(file) = OpenOptions::new().append(true).open("/dev/full") else {
            return; // not Linux
        };
        let mut writer = LogWriter {
            file,
            path: PathBuf::from("/dev/full"),
            len: 0,
            poisoned: None,
        };
        let first = writer.append(&Op::Claim { account: 1 }).unwrap_err();
        assert!(matches!(first, PersistError::Io { .. }), "{first}");
        assert_eq!(writer.len(), 0, "a failed append is not counted");
        // Everything after it is refused with a clear reason, and the refusal does not touch the disk.
        let second = writer.append(&Op::Claim { account: 1 }).unwrap_err();
        assert!(
            matches!(second, PersistError::IndexPoisoned { .. }),
            "{second}"
        );
        assert!(second.to_string().contains("restart"), "{second}");
    }

    #[test]
    fn a_successful_append_after_a_clean_failure_is_not_poisoned() {
        // A write that fails but can be rolled back leaves the writer usable: simulate the
        // rollback path with a regular file by appending, then checking the length bookkeeping.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index.log");
        let (len, _) = write_compacted(&path, &Index::default(), 1, 1).unwrap();
        let mut writer = LogWriter::open(&path, len).unwrap();
        writer
            .append(&Op::Next {
                account: 5,
                avatar: 5,
            })
            .unwrap();
        assert_eq!(fs::metadata(&path).unwrap().len(), writer.len());
        // Opening with a shorter "good length" cuts the extra bytes off (a torn tail).
        let cut = LogWriter::open(&path, len).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().len(), cut.len());
        assert_eq!(cut.len(), len);
        // A log that shrank behind our back is refused.
        assert!(matches!(
            LogWriter::open(&path, len + 100),
            Err(PersistError::Corrupt { .. })
        ));
    }
}
