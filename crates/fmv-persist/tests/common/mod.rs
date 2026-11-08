//! Helpers shared by the integration tests. Everything works on temp dirs only: nothing here
//! (or in any test) may ever touch a real state directory.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use fmv_core::AccountId;
use fmv_persist::{AvatarId, Store, INDEX_LOG_VERSION};
use serde_json::{json, Value};

pub const NOW: u64 = 1_700_000_000;

// ---- directories ---------------------------------------------------------------------------

/// A fresh state dir that does not exist yet (`open` must create it).
pub fn fresh_root() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("state");
    (dir, root)
}

pub fn open(root: &Path) -> Store {
    Store::open(root).unwrap_or_else(|e| panic!("open {}: {e}", root.display()))
}

/// Every file under `root` as relative path -> bytes. `LOCK` is excluded.
pub fn file_map(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(base, &path, out);
            } else {
                let rel = path
                    .strip_prefix(base)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                if rel != "LOCK" {
                    out.insert(rel, fs::read(&path).unwrap());
                }
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

pub fn has_temp_files(root: &Path) -> Vec<String> {
    file_map(root)
        .keys()
        .filter(|k| {
            k.rsplit('/')
                .next()
                .is_some_and(|n| n.starts_with('.') && n.contains(".tmp-"))
        })
        .cloned()
        .collect()
}

pub fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

pub fn write_json(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}

pub fn account_file(root: &Path, id: AccountId) -> PathBuf {
    root.join("accounts").join(format!("{}.json", id.0))
}

pub fn avatar_file(root: &Path, id: AvatarId) -> PathBuf {
    root.join("avatars").join(format!("{}.json", id.0))
}

pub fn progression_file(root: &Path, id: AccountId, game: &str) -> PathBuf {
    root.join("progression")
        .join(id.0.to_string())
        .join(format!("{game}.json"))
}

// ---- the index log, as the files describe it -------------------------------------------------
//
// `index.log` is `<8 hex checksum> <JSON>` per line (see the crate's `index` module); these
// helpers read and write it from outside so that the on-disk format is tested, not just used.

pub fn index_log(root: &Path) -> PathBuf {
    root.join("index.log")
}

pub fn log_checksum(payload: &str) -> String {
    blake3::hash(payload.as_bytes()).to_hex()[..8].to_string()
}

/// One checksummed line (with its newline) for `value`.
pub fn log_line(value: &Value) -> String {
    let payload = serde_json::to_string(value).unwrap();
    format!("{} {payload}\n", log_checksum(&payload))
}

pub fn log_header() -> Value {
    json!({"schema_version": INDEX_LOG_VERSION, "kind": "fmv-index-log"})
}

/// A complete log: the header, then `entries`.
pub fn log_text(entries: &[Value]) -> String {
    std::iter::once(&log_header())
        .chain(entries)
        .map(log_line)
        .collect()
}

/// Every line of `index.log` as JSON (header first), checking each checksum on the way.
pub fn log_lines(root: &Path) -> Vec<Value> {
    let text = fs::read_to_string(index_log(root)).unwrap();
    text.lines()
        .map(|line| {
            let (sum, payload) = line.split_once(' ').unwrap();
            assert_eq!(sum, log_checksum(payload), "checksum of {line:?}");
            serde_json::from_str(payload).unwrap()
        })
        .collect()
}

/// Replace the header's `schema_version` (or remove it with `None`), keeping every other line.
pub fn set_log_schema(root: &Path, value: Option<Value>) {
    let text = fs::read_to_string(index_log(root)).unwrap();
    let mut lines = log_lines(root);
    match value {
        Some(v) => lines[0]["schema_version"] = v,
        None => {
            lines[0].as_object_mut().unwrap().remove("schema_version");
        }
    }
    let rest: String = text.split_inclusive('\n').skip(1).collect();
    fs::write(index_log(root), format!("{}{rest}", log_line(&lines[0]))).unwrap();
}

pub fn account_entry(id: u64, name: &str, token_hash: &str) -> Value {
    json!({"op": "account", "id": id, "name": name, "token": token_hash, "at": NOW})
}

/// `blake3(token)` as lowercase hex: what the store keeps instead of a token.
pub fn hash_of(token: &str) -> String {
    blake3::hash(token.as_bytes()).to_hex().to_string()
}

// ---- a version 1 state directory, written by hand --------------------------------------------
//
// The version 1 layout is gone from the code, so the migration tests build it from the
// documented format: `index.json` with `names` (lower-case key -> id), `tokens` (hash -> id)
// and `avatars` (account -> avatar), plus the record files.

/// One account of a hand-built version 1 directory.
pub struct V1Account {
    pub id: u64,
    pub display_name: String,
    pub token: String,
    /// The avatar id, if the account has one.
    pub avatar: Option<u64>,
}

impl V1Account {
    pub fn new(id: u64, display_name: &str, avatar: Option<u64>) -> Self {
        Self {
            id,
            display_name: display_name.to_owned(),
            token: format!("fmvt_v1-token-of-{id}"),
            avatar,
        }
    }
}

/// Write `root` as a version 1 state directory holding `accounts`. Returns the exact bytes of
/// the `index.json` written.
pub fn write_v1_dir(
    root: &Path,
    accounts: &[V1Account],
    next_account: u64,
    next_avatar: u64,
) -> Vec<u8> {
    for sub in ["accounts", "avatars", "progression"] {
        fs::create_dir_all(root.join(sub)).unwrap();
    }
    let (mut names, mut tokens, mut avatars) = (
        serde_json::Map::new(),
        serde_json::Map::new(),
        serde_json::Map::new(),
    );
    for a in accounts {
        names.insert(a.display_name.to_lowercase(), json!(a.id));
        tokens.insert(hash_of(&a.token), json!(a.id));
        write_json(
            &root.join("accounts").join(format!("{}.json", a.id)),
            &json!({
                "schema_version": 1,
                "account_id": a.id,
                "display_name": a.display_name,
                "token_hash": hash_of(&a.token),
                "created_at": NOW + a.id,
            }),
        );
        if let Some(avatar) = a.avatar {
            avatars.insert(a.id.to_string(), json!(avatar));
            write_json(
                &root.join("avatars").join(format!("{avatar}.json")),
                &json!({
                    "schema_version": 1,
                    "avatar_id": avatar,
                    "account_id": a.id,
                    "display_name": a.display_name,
                    "tint": [1, 2, 3],
                    "base_archetype": "default",
                    "per_game_overrides": {},
                    "created_at": NOW + a.id,
                    "updated_at": NOW + a.id,
                }),
            );
        }
    }
    let index = json!({
        "schema_version": 1,
        "next_account_id": next_account,
        "next_avatar_id": next_avatar,
        "names": names,
        "tokens": tokens,
        "avatars": avatars,
    });
    let mut bytes = serde_json::to_vec_pretty(&index).unwrap();
    bytes.push(b'\n');
    fs::write(root.join("index.json"), &bytes).unwrap();
    bytes
}
