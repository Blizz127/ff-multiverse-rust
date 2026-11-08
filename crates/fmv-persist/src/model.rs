//! Record types and limits.
//!
//! Records hold references only (ids, names, opaque adapter blobs). Nothing here ever holds
//! game asset bytes.

use std::collections::BTreeMap;
use std::fmt;

use fmv_core::{AccountId, AvatarAppearance, GameId};
use serde::{Deserialize, Serialize};

use crate::error::{PersistError, Result};

/// Record format version written by this build. Loaders accept `1..=SCHEMA_VERSION` and
/// reject anything newer with [`PersistError::FutureSchema`]. This versions the account,
/// avatar and progression record files; the index has its own [`INDEX_LOG_VERSION`].
pub const SCHEMA_VERSION: u32 = 1;
/// Format version of the account index (`index.log`, and the small `index.json` marker beside
/// it). Version 1 was a single `index.json` holding every name and token; it is migrated
/// transparently on open. An older build that finds the marker refuses to open the directory
/// with [`PersistError::FutureSchema`] instead of misreading it.
pub const INDEX_LOG_VERSION: u32 = 2;
/// Largest accepted account/avatar/progression record file.
pub const MAX_RECORD_BYTES: u64 = 1024 * 1024;
/// Largest `index.json` that is read: the pre-v2 index (migrated on open) and the v2 marker.
/// The v2 index itself (`index.log`) has no total size cap, so the number of accounts is no
/// longer limited by it; what bounds it is [`StoreOptions::max_accounts`](crate::StoreOptions)
/// and a per-line cap while it is read. (A v1 index cost about 140 bytes per account and
/// stopped accepting registrations for good at about 121,000 accounts.)
pub const MAX_INDEX_BYTES: u64 = 16 * 1024 * 1024;
/// Largest accepted opaque progression blob (bytes, before hex encoding). Note that a blob
/// this large cannot travel in a protocol frame (a frame holds 256 KiB, and a blob that is
/// serialized as a list of numbers is several times its size), so a server must keep it in the
/// store and put only a short key of it (an id, a hash) on the wire.
pub const MAX_BLOB_BYTES: usize = 256 * 1024;
/// Largest accepted [`CharacterProgress`] record file. A record is a few hundred bytes; this is
/// the cap a corrupt or hostile file is held to, on read and on write.
pub const MAX_CHARACTER_RECORD_BYTES: u64 = 4 * 1024;
/// Longest accepted `base_archetype` (bytes).
pub const MAX_ARCHETYPE_BYTES: usize = 64;
/// Longest accepted per-game appearance reference, in **characters**: exactly the limit the
/// server applies to a client's `appearance_ref` (`AvatarAppearance::MAX_APPEARANCE_REF`).
/// 64 four-byte characters (256 bytes) are valid. Share this constant instead of copying the
/// number: a limit counted in different units on the two sides is what used to burn a name when
/// the server accepted a reference that the store refused.
///
/// Only the length and control characters are checked: a reference is an opaque, adapter-defined
/// token (it may contain `/`, `\` or `..`), stored and shown to other players as it is. An
/// adapter must validate it against what it knows before it ever names a model or a file.
pub const MAX_APPEARANCE_REF_CHARS: usize = AvatarAppearance::MAX_APPEARANCE_REF;
/// Longest per-game appearance reference of the older, byte-counted rule. Newly registered
/// references are limited by [`MAX_APPEARANCE_REF_CHARS`]; [`Store::put_avatar`](crate::Store)
/// still accepts a stored reference up to this many bytes so that an avatar written by an
/// older build can be updated.
pub const MAX_APPEARANCE_REF_BYTES: usize = 128;
/// Longest token string `authenticate` will even hash.
pub const MAX_TOKEN_BYTES: usize = 256;

/// Identifies an [`AvatarProfile`]. Local to this crate because core has no avatar id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AvatarId(pub u64);

impl fmt::Display for AvatarId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "avatar#{}", self.0)
    }
}

/// Stable string form of a [`GameId`] used as map key and file name: the serde variant name
/// ("Ff7", "Fixture", ...). New core variants work without a change here.
pub fn game_key(game: GameId) -> String {
    match serde_json::to_value(game) {
        Ok(serde_json::Value::String(s)) => s,
        // Unit variants always serialize to strings; this keeps the function total.
        _ => format!("{game:?}"),
    }
}

/// Inverse of [`game_key`]. `None` for strings that are not a known game.
pub fn parse_game_key(key: &str) -> Option<GameId> {
    serde_json::from_value(serde_json::Value::String(key.to_owned())).ok()
}

/// A key is safe to use as a file name component.
pub(crate) fn is_safe_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 32
        && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Public view of an account. The token hash never leaves the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountInfo {
    pub account_id: AccountId,
    pub display_name: String,
    pub created_at: u64,
}

/// On-disk account record: `accounts/<account_id>.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AccountRecord {
    pub schema_version: u32,
    pub account_id: AccountId,
    /// Sanitized, case preserved. Uniqueness is on `fmv_core::name_key` of it (see the index).
    pub display_name: String,
    /// blake3(token) as lowercase hex. The plaintext token is never stored.
    pub token_hash: String,
    pub created_at: u64,
}

/// Persistent avatar: references only, never mesh or texture bytes.
///
/// `display_name` always has the same `fmv_core::name_key` as the owning account's display
/// name (so it can differ in case only); the store enforces this so in-world names stay unique.
/// Renaming is not supported yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AvatarProfile {
    /// Record format version. The store stamps [`SCHEMA_VERSION`] on every write.
    pub schema_version: u32,
    pub avatar_id: AvatarId,
    pub account_id: AccountId,
    pub display_name: String,
    pub tint: [u8; 3],
    /// Game-agnostic body choice; each adapter maps it to its closest model.
    pub base_archetype: String,
    /// [`game_key`] of a game -> adapter-defined appearance id (e.g. an FF7 field model name).
    #[serde(default)]
    pub per_game_overrides: BTreeMap<String, String>,
    pub created_at: u64,
    pub updated_at: u64,
}

impl AvatarProfile {
    /// Override for `game`, if one is set.
    pub fn override_for(&self, game: GameId) -> Option<&str> {
        self.per_game_overrides
            .get(&game_key(game))
            .map(String::as_str)
    }

    /// Set (`Some`) or clear (`None`) the override for `game`. Persist with `put_avatar`.
    pub fn set_override(&mut self, game: GameId, appearance_ref: Option<String>) {
        let key = game_key(game);
        match appearance_ref {
            Some(r) => {
                self.per_game_overrides.insert(key, r);
            }
            None => {
                self.per_game_overrides.remove(&key);
            }
        }
    }

    /// Resolve this profile for one game. `appearance_ref` is the per-game override, or
    /// `None` when there is none (the adapter then falls back to `base_archetype`).
    pub fn appearance_for(&self, game: GameId) -> AvatarAppearance {
        AvatarAppearance {
            display_name: self.display_name.clone(),
            tint: self.tint,
            appearance_ref: self.override_for(game).map(str::to_owned),
        }
    }

    /// Field checks that do not need store state. Names are checked by the store.
    pub(crate) fn validate_fields(&self) -> Result<()> {
        check_text("base_archetype", &self.base_archetype, MAX_ARCHETYPE_BYTES)?;
        // One entry per game at most, so this also bounds the map.
        for (key, value) in &self.per_game_overrides {
            if parse_game_key(key).is_none() {
                return Err(PersistError::InvalidField {
                    field: "per_game_overrides",
                    reason: format!("{key:?} is not a known game"),
                });
            }
            check_stored_appearance_ref(value)?;
        }
        Ok(())
    }
}

/// Non-empty, bounded, no control characters.
fn check_text(field: &'static str, value: &str, max_bytes: usize) -> Result<()> {
    if value.is_empty() {
        return Err(PersistError::InvalidField {
            field,
            reason: "must not be empty".into(),
        });
    }
    if value.len() > max_bytes {
        return Err(PersistError::InvalidField {
            field,
            reason: format!("is {} bytes, over the {max_bytes} byte limit", value.len()),
        });
    }
    if value.chars().any(char::is_control) {
        return Err(PersistError::InvalidField {
            field,
            reason: "must not contain control characters".into(),
        });
    }
    Ok(())
}

/// The rule for a **new** per-game appearance reference (registration): exactly what the
/// server accepts, [`AvatarAppearance::is_valid_appearance_ref`]. Nothing stricter, so a value
/// the server cleaned can never be refused here.
pub(crate) fn check_new_appearance_ref(value: &str) -> Result<()> {
    if value.is_empty() {
        return Err(PersistError::InvalidField {
            field: "appearance_ref",
            reason: "must not be empty".into(),
        });
    }
    let chars = value.chars().count();
    if chars > MAX_APPEARANCE_REF_CHARS {
        return Err(PersistError::InvalidField {
            field: "appearance_ref",
            reason: format!(
                "is {chars} characters, over the {MAX_APPEARANCE_REF_CHARS} character limit"
            ),
        });
    }
    if value.chars().any(char::is_control) {
        return Err(PersistError::InvalidField {
            field: "appearance_ref",
            reason: "must not contain control characters".into(),
        });
    }
    Ok(())
}

/// The rule for a reference in an avatar that is being stored again (`put_avatar`): the new
/// character rule, or the older byte rule, so that an avatar written by a build that allowed
/// up to [`MAX_APPEARANCE_REF_BYTES`] ASCII characters stays updatable.
fn check_stored_appearance_ref(value: &str) -> Result<()> {
    let older_rule_ok = !value.is_empty()
        && value.len() <= MAX_APPEARANCE_REF_BYTES
        && !value.chars().any(char::is_control);
    if older_rule_ok {
        return Ok(());
    }
    check_new_appearance_ref(value).map_err(|e| match e {
        PersistError::InvalidField { reason, .. } => PersistError::InvalidField {
            field: "per_game_overrides",
            reason,
        },
        other => other,
    })
}

/// What [`Store::register`](crate::Store::register) returns: the new account, the one-time
/// plaintext token, and the avatar that was created with it.
#[derive(Clone, PartialEq, Eq)]
pub struct Registered {
    pub account_id: AccountId,
    /// Shown exactly once: only `blake3(token)` is stored. `Debug` does not print it.
    pub token: String,
    pub avatar: AvatarProfile,
}

impl fmt::Debug for Registered {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Registered")
            .field("account_id", &self.account_id)
            .field("token", &"<redacted>")
            .field("avatar", &self.avatar)
            .finish()
    }
}

/// One account's progression for one game. The blob is opaque to core and to this crate;
/// only the game adapter interprets it. Games never see each other's progression.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progression {
    /// Record format version. The store stamps [`SCHEMA_VERSION`] on every write.
    pub schema_version: u32,
    pub account_id: AccountId,
    pub game: GameId,
    /// Stored as a hex string on disk.
    #[serde(with = "hex_blob")]
    pub blob: Vec<u8>,
    pub updated_at: u64,
}

impl Progression {
    pub fn new(account_id: AccountId, game: GameId, blob: Vec<u8>, updated_at: u64) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            account_id,
            game,
            blob,
            updated_at,
        }
    }

    /// What `get_progression` returns when nothing was stored yet.
    pub fn empty(account_id: AccountId, game: GameId) -> Self {
        Self::new(account_id, game, Vec::new(), 0)
    }
}

/// One account's character progress for one game: which of the game's characters it plays, and
/// what that character has earned. **Not** [`Progression`]: that blob is the account's story
/// state and also the key of its story phase (the server builds a `PhaseKey` from it), so
/// battle results written into it would move the player to another instance. This record is
/// separate, has its own file and API, and no adapter reads a phase from it.
///
/// The store is game-agnostic: `character` is the game's own character id (FF7: Cloud 0,
/// Barret 1, ...), and the numbers mean whatever the game says. The caps and curves of a game
/// (FF7's level 99, its EXP table, its gil limit) belong to its adapter, which applies them
/// before it stores. The store only checks what is true in every game: the record belongs to
/// the account and game it is filed under, `level` is at least 1, the schema is one it knows and
/// the file is small.
///
/// A record that was never stored is absent (`Store::get_character_progress` returns `None`),
/// not a default: the starting values come from the game's own data, which only the adapter
/// has, so there is nothing to migrate when a game starts using this record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CharacterProgress {
    /// Record format version. The store stamps [`SCHEMA_VERSION`] on every write.
    pub schema_version: u32,
    pub account_id: AccountId,
    pub game: GameId,
    /// The game's id of the character the account plays (opaque here).
    pub character: u8,
    /// At least 1.
    pub level: u8,
    /// Total experience.
    pub exp: u32,
    /// Ability points earned.
    pub ap: u32,
    /// Money.
    pub gil: u32,
    pub updated_at: u64,
}

impl CharacterProgress {
    /// A fresh record: `character` at `level` with no experience, AP or gil (an adapter that
    /// starts a character with experience sets `exp` itself).
    pub fn new(
        account_id: AccountId,
        game: GameId,
        character: u8,
        level: u8,
        updated_at: u64,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            account_id,
            game,
            character,
            level,
            exp: 0,
            ap: 0,
            gil: 0,
            updated_at,
        }
    }

    /// What is true of every game's record (see the type's documentation).
    pub(crate) fn validate(&self) -> Result<()> {
        if self.level == 0 {
            return Err(PersistError::InvalidField {
                field: "level",
                reason: "must be at least 1".into(),
            });
        }
        Ok(())
    }
}

/// `Vec<u8>` <-> hex string, rejecting oversized input before decoding it.
mod hex_blob {
    use super::MAX_BLOB_BYTES;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(blob: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(blob))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(d)?;
        if text.len() > MAX_BLOB_BYTES.saturating_mul(2) {
            return Err(serde::de::Error::custom(format!(
                "blob is {} hex characters, over the {} byte limit",
                text.len(),
                MAX_BLOB_BYTES
            )));
        }
        hex::decode(&text).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_GAMES: [GameId; 6] = [
        GameId::Ff7,
        GameId::Ff8,
        GameId::Ff9,
        GameId::Ff10,
        GameId::Ff12,
        GameId::Fixture,
    ];

    #[test]
    fn game_keys_are_stable_safe_and_roundtrip() {
        let keys: Vec<String> = ALL_GAMES.iter().map(|g| game_key(*g)).collect();
        assert_eq!(keys, ["Ff7", "Ff8", "Ff9", "Ff10", "Ff12", "Fixture"]);
        for (game, key) in ALL_GAMES.iter().zip(&keys) {
            assert!(is_safe_key(key), "{key}");
            assert_eq!(parse_game_key(key), Some(*game));
        }
        assert_eq!(parse_game_key("ff7"), None);
        assert_eq!(parse_game_key("../x"), None);
        assert_eq!(parse_game_key(""), None);
    }

    #[test]
    fn unsafe_keys_are_rejected() {
        for bad in ["", "a/b", "..", "a.b", "a b", "ü", &"x".repeat(33)] {
            assert!(!is_safe_key(bad), "{bad:?}");
        }
    }

    #[test]
    fn blob_hex_roundtrip_and_rejection() {
        let p = Progression::new(AccountId(3), GameId::Ff7, vec![0, 1, 0xfe, 0xff], 9);
        let text = serde_json::to_string(&p).unwrap();
        assert!(text.contains("\"0001feff\""), "{text}");
        assert_eq!(serde_json::from_str::<Progression>(&text).unwrap(), p);

        for bad in ["\"abc\"", "\"zz\"", "7", "null"] {
            let doc = format!(
                "{{\"schema_version\":1,\"account_id\":3,\"game\":\"Ff7\",\"blob\":{bad},\"updated_at\":0}}"
            );
            assert!(serde_json::from_str::<Progression>(&doc).is_err(), "{bad}");
        }
        let huge = "ab".repeat(MAX_BLOB_BYTES + 1);
        let doc = format!(
            "{{\"schema_version\":1,\"account_id\":3,\"game\":\"Ff7\",\"blob\":\"{huge}\",\"updated_at\":0}}"
        );
        assert!(serde_json::from_str::<Progression>(&doc).is_err());
    }
}
