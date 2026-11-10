//! Record types and limits.
//!
//! Records hold references only (ids, names, opaque adapter blobs). Nothing here ever holds
//! game asset bytes.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use fmv_core::{AccountId, AvatarAppearance, GameId, InventoryEntry, MAX_INVENTORY_STACKS};
use serde::{Deserialize, Serialize};

use crate::character::PlayerCharacter;
use crate::error::{PersistError, Result};

/// Record format version written by this build. Loaders accept `1..=SCHEMA_VERSION` and
/// reject anything newer with [`PersistError::FutureSchema`]. This versions the account,
/// avatar and progression record files; the index has its own [`INDEX_LOG_VERSION`].
///
/// An optional field that reads as "absent" when it is missing is additive and does not change
/// the version: [`CharacterProgress::inventory`] was added that way. (What that costs a build
/// that is rolled back is documented there.)
pub const SCHEMA_VERSION: u32 = 1;
/// Default format version of a new account index (`index.log`, and the small `index.json`
/// marker beside it). Scene batches raise the log header to version 3 before their first
/// write, while retaining the legacy marker at version 2. Version 1 was a single
/// `index.json` holding every name and token; it is migrated
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
/// Largest accepted character-progress file, including its private scene receipts and checkpoint.
/// The original fields and a full inventory use under 18 KiB. Up to
/// [`MAX_SCENE_RECEIPTS`](crate::MAX_SCENE_RECEIPTS) durable receipt hashes fit alongside them;
/// those receipts are never evicted. Ordinary receipts require schema 2; batch receipts,
/// checkpoints and reservations require schema 3. The first batch also upgrades the index
/// header so older servers refuse the entire store. Records without metadata retain schema 1.
/// A checkpoint's replay has up to 256 KiB of bytes; pretty JSON puts each on an indented
/// line. This cap includes that expansion, a complete inventory and all 2,048 receipts.
/// A conservative character bound is `12 * 256 KiB + 16 * (2 * 8 KiB)` for replay,
/// original story and landing arrays, `256 * 2048` for receipts, and 32 KiB for the
/// remaining fields: 3,964,928 bytes. The actual widest validated envelope is exercised
/// in `store::scene::batch::tests::size_bounds`, including the deeper intent indentation.
/// Raising the bound changes no serialization, receipt identity or recovery checksum.
pub const MAX_CHARACTER_RECORD_BYTES: u64 = 4 * 1024 * 1024;
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
    /// An untouched registration seed is still an orphan if its token was never delivered.
    /// The digest binds the complete initial file, including its account id and timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registration_progress: Option<RegistrationProgress>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RegistrationProgress {
    pub game: GameId,
    pub digest: [u8; 32],
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
/// plaintext token, and the avatar that was created with it (and, from
/// [`Store::register_with_character`](crate::Store::register_with_character), its player-character
/// record).
#[derive(Clone, PartialEq, Eq)]
pub struct Registered {
    pub account_id: AccountId,
    /// Shown exactly once: only `blake3(token)` is stored. `Debug` does not print it.
    pub token: String,
    pub avatar: AvatarProfile,
    /// The player-character record created with the account, as stored. `None` for a plain `register`.
    pub character: Option<PlayerCharacter>,
}

impl fmt::Debug for Registered {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Registered")
            .field("account_id", &self.account_id)
            .field("token", &"<redacted>")
            .field("avatar", &self.avatar)
            .field("character", &self.character)
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

/// One account's character progress for one game: which of the game's characters it plays, what
/// that character has earned (level, experience, AP, gil) and, once the game stores one, what the
/// account carries (the inventory). **Not** [`Progression`]: that blob is the account's story
/// state and also the key of its story phase (the server builds a `PhaseKey` from it), so
/// battle results written into it would move the player to another instance. This record is
/// separate, has its own file and API, and no adapter reads a phase from it.
///
/// The store is game-agnostic: `character` is the game's own character id (FF7: Cloud 0,
/// Barret 1, ...), and the numbers mean whatever the game says. The caps and curves of a game
/// (FF7's level 99, its EXP table, its gil limit, its item table and stack sizes) belong to its
/// adapter, which applies them before it stores. The store only checks what is true in every
/// game: the record belongs to the account and game it is filed under, `level` is at least 1,
/// the inventory (if there is one) is well formed (see [`CharacterProgress::inventory`]), the
/// schema is one it knows and the file is small ([`MAX_CHARACTER_RECORD_BYTES`]).
///
/// A record that was never stored is absent (`Store::get_character_progress` returns `None`),
/// not a default: the starting values come from the game's own data, which only the adapter
/// has, so there is nothing to migrate when a game starts using this record. The same goes one
/// level down: a record that is there but has no inventory is not an empty inventory (see
/// [`CharacterProgress::inventory`]).
///
/// Every write replaces the whole record. A record built with [`CharacterProgress::new`] has no
/// inventory, so putting it **clears a stored one**; to change the numbers and keep the items,
/// change the record that was read (`Store::update_character_progress` hands it to the closure).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CharacterProgress {
    /// Record format version. The store stamps the base [`SCHEMA_VERSION`], or its private
    /// receipt/checkpoint envelope's version (2 or 3); callers cannot downgrade that metadata.
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
    /// What the account carries: one [`InventoryEntry`] per item. Item ids and counts are the
    /// game's own (FF7: an index into the kernel's item table) and mean nothing here; the order
    /// is the caller's and is kept as it is.
    ///
    /// **`None` and `Some(vec![])` are different.** `None` means "never stored": the caller (the
    /// server) then starts from its game's own starting stock, which only the game's adapter and
    /// the server know. The store never makes one up and never fills in a default. `Some(vec![])`
    /// is a real, stored, empty inventory (the account has used or sold everything) and stays
    /// empty: it is not replaced by the starting stock. On disk `None` is a missing `inventory`
    /// key, so a record that has none is byte for byte what a build from before this field wrote,
    /// and `Some(vec![])` is `"inventory": []`. (A JSON `null` reads as `None`, like a missing
    /// key.)
    ///
    /// Checked on every read and write: at most [`MAX_INVENTORY_STACKS`] entries, no entry with a
    /// `count` of 0 (an item the account does not have is not listed) and no item twice. A file
    /// that breaks them is [`PersistError::Corrupt`] on read; a record that does is refused with
    /// [`PersistError::InvalidField`] (`field: "inventory"`) on write, and the old record stays.
    /// A game's own limits (its item table, its stack size) are its adapter's business.
    ///
    /// # Compatibility
    ///
    /// Older records have no such key and read as `None`, and `SCHEMA_VERSION` stays 1: the field
    /// is additive. The reverse is not free. A build from before this field ignores the key when
    /// it reads, and **drops the inventory when it next rewrites the record** (a battle result,
    /// say). It also held a record to 4 KiB (see [`MAX_CHARACTER_RECORD_BYTES`]), so it refuses
    /// one of more than 70 to 80 stacks (about 50 bytes each, 160 to 180 bytes for the rest) as
    /// [`PersistError::TooLarge`] instead of ignoring the key, which fails that account's login.
    /// Going back to such a build is safe only from a snapshot taken before any inventory was
    /// stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inventory: Option<Vec<InventoryEntry>>,
    /// The character's limit gauge between battles, in the game's own units (FF7: `0..=255`, 255 is
    /// full): what the account's character fights its next battle with, saved at the end of every
    /// one. Opaque here, like the other numbers: the store checks no maximum (a game's own limits
    /// are its adapter's business, and the server clamps what it reads).
    ///
    /// # Compatibility
    ///
    /// Older records have no such key and read as 0 (an empty gauge, which is also what a new
    /// character has), and `SCHEMA_VERSION` stays 1: the field is additive. A record whose gauge is 0
    /// is written **without** the key, so it is byte for byte what a build from before this field
    /// wrote. The reverse costs one thing: a build from before this field ignores the key when it
    /// reads and **drops the gauge when it next rewrites the record** (a battle result, say), so the
    /// character comes back with an empty gauge, nothing else lost. A value outside `0..=65535`
    /// is not a `u16`: the record is [`PersistError::Corrupt`] on read, never clamped.
    #[serde(default, skip_serializing_if = "is_empty_gauge")]
    pub limit: u16,
}

/// Game-provided initial character progress for an atomic registration. The store supplies
/// the new account id, record version and creation time; callers cannot seed another account.
/// The inventory retains the ordinary `None` (never stored) versus `Some([])` distinction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewCharacterProgress {
    pub game: GameId,
    pub character: u8,
    pub level: u8,
    pub exp: u32,
    pub ap: u32,
    pub gil: u32,
    pub limit: u16,
    pub inventory: Option<Vec<InventoryEntry>>,
}

impl NewCharacterProgress {
    /// A character at `level`, with zero balances and no stored inventory.
    pub fn new(game: GameId, character: u8, level: u8) -> Self {
        Self { game, character, level, exp: 0, ap: 0, gil: 0, limit: 0, inventory: None }
    }

    pub(crate) fn record(&self, account: AccountId, now: u64) -> CharacterProgress {
        CharacterProgress { exp: self.exp, ap: self.ap, gil: self.gil, limit: self.limit, inventory: self.inventory.clone(),
            ..CharacterProgress::new(account, self.game, self.character, self.level, now) }
    }
}

/// `skip_serializing_if` of [`CharacterProgress::limit`]: an empty gauge is not written.
fn is_empty_gauge(limit: &u16) -> bool {
    *limit == 0
}

impl CharacterProgress {
    /// A fresh record: `character` at `level` with no experience, AP or gil (an adapter that
    /// starts a character with experience sets `exp` itself) and **no inventory** (`None`: the
    /// game's starting stock applies). Putting it replaces a stored record whole, so it also
    /// clears a stored inventory; see the type's documentation.
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
            inventory: None,
            limit: 0,
        }
    }

    /// What is true of every game's record (see the type's documentation). Checked on every read
    /// (a failure is `Corrupt`) and every write (a failure is `InvalidField`).
    pub(crate) fn validate(&self) -> Result<()> {
        if self.level == 0 {
            return Err(PersistError::InvalidField {
                field: "level",
                reason: "must be at least 1".into(),
            });
        }
        if let Some(inventory) = &self.inventory {
            check_inventory(inventory)?;
        }
        Ok(())
    }
}

/// The checks every game's inventory shares: bounded, no empty stack, no item twice. The length
/// is checked first, so a caller's oversized list costs nothing more than that comparison.
fn check_inventory(inventory: &[InventoryEntry]) -> Result<()> {
    if inventory.len() > MAX_INVENTORY_STACKS {
        return Err(PersistError::InvalidField {
            field: "inventory",
            reason: format!(
                "has {} stacks, over the {MAX_INVENTORY_STACKS} stack limit",
                inventory.len()
            ),
        });
    }
    let mut seen = BTreeSet::new();
    for entry in inventory {
        if entry.count == 0 {
            return Err(PersistError::InvalidField {
                field: "inventory",
                reason: format!("item {} has a stack of zero", entry.item),
            });
        }
        if !seen.insert(entry.item) {
            return Err(PersistError::InvalidField {
                field: "inventory",
                reason: format!("item {} appears more than once", entry.item),
            });
        }
    }
    Ok(())
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

    // ---- the inventory of a CharacterProgress ----------------------------------------------

    fn with_inventory(inventory: Option<Vec<InventoryEntry>>) -> CharacterProgress {
        CharacterProgress {
            inventory,
            ..CharacterProgress::new(AccountId(1), GameId::Ff7, 0, 5, 0)
        }
    }

    fn stacks(items: impl IntoIterator<Item = (u16, u16)>) -> Vec<InventoryEntry> {
        items
            .into_iter()
            .map(|(item, count)| InventoryEntry::new(item, count))
            .collect()
    }

    /// The reason a record is refused for its inventory (and that the refusal names that field).
    fn refusal(record: &CharacterProgress) -> String {
        match record.validate() {
            Err(PersistError::InvalidField {
                field: "inventory",
                reason,
            }) => reason,
            other => panic!("expected the inventory to be refused, got {other:?}"),
        }
    }

    #[test]
    fn a_new_record_has_no_inventory_and_none_is_not_empty() {
        let fresh = CharacterProgress::new(AccountId(1), GameId::Ff7, 0, 5, 0);
        assert_eq!(fresh.inventory, None);

        // On disk: None is a missing key, Some(empty) is an empty list, never the other way round.
        let none = with_inventory(None);
        let empty = with_inventory(Some(vec![]));
        let some = with_inventory(Some(stacks([(3, 99), (1, 2)])));
        let none_json = serde_json::to_value(&none).unwrap();
        assert_eq!(none_json.get("inventory"), None, "{none_json}");
        assert_eq!(
            serde_json::to_value(&empty).unwrap()["inventory"],
            serde_json::json!([])
        );
        assert_eq!(
            serde_json::to_value(&some).unwrap()["inventory"],
            serde_json::json!([{"item": 3, "count": 99}, {"item": 1, "count": 2}])
        );
        // Each reads back as it was written: the distinction and the caller's order survive.
        for record in [&none, &empty, &some] {
            let text = serde_json::to_string(record).unwrap();
            assert_eq!(
                &serde_json::from_str::<CharacterProgress>(&text).unwrap(),
                record,
                "{text}"
            );
        }

        // A record from a build that did not know the field has no key and reads as None; so does
        // a JSON null (nothing was stored); `[]` is a stored, empty inventory.
        let read = |inventory: Option<serde_json::Value>| {
            let mut doc = none_json.clone();
            if let Some(value) = inventory {
                doc["inventory"] = value;
            }
            serde_json::from_value::<CharacterProgress>(doc)
                .unwrap()
                .inventory
        };
        assert_eq!(read(None), None);
        assert_eq!(read(Some(serde_json::Value::Null)), None);
        assert_eq!(read(Some(serde_json::json!([]))), Some(vec![]));
    }

    /// The limit gauge was added to records that exist on a running server: an empty gauge is no key at
    /// all (byte for byte a record of the build before), a record without the key reads as empty, and
    /// the gauge round-trips with the inventory beside it.
    #[test]
    fn the_limit_gauge_is_a_missing_key_when_empty_and_a_record_without_it_reads_as_empty() {
        let fresh = CharacterProgress::new(AccountId(1), GameId::Ff7, 0, 5, 0);
        assert_eq!(fresh.limit, 0);
        let empty_json = serde_json::to_value(&fresh).unwrap();
        assert_eq!(empty_json.get("limit"), None, "{empty_json}");

        // A record of the v6 build (this one's JSON without the key) loads with an empty gauge.
        let v6 = serde_json::from_value::<CharacterProgress>(empty_json.clone()).unwrap();
        assert_eq!(v6, fresh);

        // A gauge is written and read back, next to an inventory, and survives `Some(vec![])` too.
        for limit in [1u16, 137, 255, u16::MAX] {
            for inventory in [None, Some(vec![]), Some(stacks([(3, 9)]))] {
                let record = CharacterProgress { limit, ..with_inventory(inventory) };
                let json = serde_json::to_value(&record).unwrap();
                assert_eq!(json["limit"], serde_json::json!(limit));
                assert_eq!(serde_json::from_value::<CharacterProgress>(json).unwrap(), record);
                assert!(record.validate().is_ok(), "the store checks no maximum: that is the game's business");
            }
        }

        // Not a u16: the record does not read (and is not clamped into one that does).
        for bad in [serde_json::json!(70_000), serde_json::json!(-1), serde_json::json!("full"), serde_json::json!(1.5)] {
            let mut doc = empty_json.clone();
            doc["limit"] = bad.clone();
            assert!(serde_json::from_value::<CharacterProgress>(doc).is_err(), "{bad}");
        }
    }

    #[test]
    fn an_inventory_is_checked_for_size_empty_stacks_and_repeats() {
        // Fine: never stored, stored empty, a few stacks in any order, the widest numbers.
        for ok in [
            None,
            Some(vec![]),
            Some(stacks([(9, 1), (2, 99), (300, 1)])),
            Some(stacks([(0, 1), (u16::MAX, u16::MAX)])),
        ] {
            assert!(with_inventory(ok.clone()).validate().is_ok(), "{ok:?}");
        }

        // Exactly the limit is fine; one more stack is not.
        let limit = u16::try_from(MAX_INVENTORY_STACKS).unwrap();
        let full = stacks((0..limit).map(|item| (item, 1)));
        assert_eq!(full.len(), MAX_INVENTORY_STACKS);
        assert!(with_inventory(Some(full.clone())).validate().is_ok());
        let mut over = full;
        over.push(InventoryEntry::new(limit, 1));
        let why = refusal(&with_inventory(Some(over)));
        assert!(why.contains("321 stacks") && why.contains("320"), "{why}");

        // A huge list is refused by its length alone (no pass over it, no mention of its content).
        let huge = vec![InventoryEntry::new(0, 0); 100_000];
        let why = refusal(&with_inventory(Some(huge)));
        assert!(
            why.contains("100000 stacks") && !why.contains("zero"),
            "{why}"
        );

        // A stack of zero is an item the account does not have: it is not listed.
        let why = refusal(&with_inventory(Some(stacks([(1, 1), (2, 0)]))));
        assert!(why.contains("item 2") && why.contains("zero"), "{why}");

        // An item twice, wherever the two stacks are (the counts are not added up here).
        let why = refusal(&with_inventory(Some(stacks([(1, 1), (2, 1), (1, 5)]))));
        assert!(
            why.contains("item 1") && why.contains("more than once"),
            "{why}"
        );
        refusal(&with_inventory(Some(stacks([(7, 3), (7, 3)]))));

        // The other checks still apply to a record that has an inventory.
        let zero_level = CharacterProgress {
            level: 0,
            ..with_inventory(Some(stacks([(1, 1)])))
        };
        assert!(matches!(
            zero_level.validate(),
            Err(PersistError::InvalidField { field: "level", .. })
        ));
    }

    /// The record cap must stay above the largest *valid* record, or the store could accept a
    /// record on `put` that it then refuses to write (or one that it wrote and cannot read).
    /// `MAX_INVENTORY_STACKS` lives in `fmv-core`: this is where raising it is noticed.
    #[test]
    fn the_largest_valid_record_fits_the_record_cap_with_room_to_spare() {
        // The widest numbers everywhere: five-digit items and counts, 64-bit ids and times.
        let widest = CharacterProgress {
            schema_version: u32::MAX,
            account_id: AccountId(u64::MAX),
            game: GameId::Fixture,
            character: u8::MAX,
            level: u8::MAX,
            exp: u32::MAX,
            ap: u32::MAX,
            gil: u32::MAX,
            updated_at: u64::MAX,
            inventory: Some(
                (0..MAX_INVENTORY_STACKS)
                    .map(|i| InventoryEntry::new(u16::MAX - u16::try_from(i).unwrap(), u16::MAX))
                    .collect(),
            ),
            limit: u16::MAX,
        };
        assert!(widest.validate().is_ok());
        // What `fsio::write_json` writes: pretty-printed, plus a final newline.
        let bytes = serde_json::to_vec_pretty(&widest).unwrap().len() as u64 + 1;
        assert!(
            bytes < MAX_CHARACTER_RECORD_BYTES,
            "{bytes} bytes against a cap of {MAX_CHARACTER_RECORD_BYTES}"
        );
        // Room to grow by half again, so that a field added later does not leave it on the edge.
        assert!(
            bytes * 3 / 2 < MAX_CHARACTER_RECORD_BYTES,
            "{bytes} bytes is too close to the cap of {MAX_CHARACTER_RECORD_BYTES}"
        );
        // It is more than the 4 KiB that the cap was before the inventory: that is why it grew.
        assert!(bytes > 4 * 1024, "{bytes}");
    }
}
