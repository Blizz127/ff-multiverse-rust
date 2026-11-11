//! Where a player last stood in a game: one small record per (account, game), written constantly
//! (MMO-style saving: no save points) and read at login to put the player back where it left.
//!
//! ```text
//! <root>/progression/<account_id>/<Game>.location.json   SavedLocation
//! ```
//!
//! * **Older builds ignore it.** They only ever open `<Game>.json` and `<Game>.character.json` in that
//!   directory and judge the directory by its name alone, so rolling back to a build without this
//!   record changes nothing for them (the account simply joins where it always joined).
//! * **It goes where the account goes.** Snapshots copy, and a character delete moves, the whole
//!   `progression/<account_id>/` directory.
//! * **It is never the story.** The story (the progression blob) and the character's progress keep their
//!   own records and their own writers. This record only says where to stand, and carries a copy of the
//!   story it was saved with so that a login can tell a spot saved under the same story from a stale one.
//! * **Its writes take the store's writer lock** and refuse an account the index no longer has, so a
//!   write that was queued before a delete can never bring the deleted account's directory back.

use std::path::PathBuf;

use fmv_core::{AccountId, GameId};
use serde::{Deserialize, Serialize};

use super::Store;
use crate::error::{PersistError, Result};
use crate::fsio;
use crate::model::{game_key, is_safe_key, SCHEMA_VERSION};

/// Largest accepted location record file, on read and on write. A record is about 300 bytes plus the
/// story copy (hex: two bytes per story byte), so this leaves room for a long story.
pub const MAX_LOCATION_RECORD_BYTES: u64 = 64 * 1024;
/// Longest accepted area key (bytes). FF7's field names are eight characters at most.
pub const MAX_LOCATION_AREA_BYTES: usize = 64;
/// Largest story copy kept in a location record (bytes, before hex).
pub const MAX_LOCATION_STORY_BYTES: usize = 16 * 1024;

/// Where an account's player last stood in a game.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedLocation {
    /// Record format version. The store stamps [`SCHEMA_VERSION`] on every write.
    pub schema_version: u32,
    pub account_id: AccountId,
    pub game: GameId,
    /// The area the player stood in (the server's area key: an FF7 field name).
    pub area: String,
    /// The position in that area's own coordinates, height included (FF7 picks the floor of a spot
    /// where floors overlap by the height it is asked about).
    pub pos: [f32; 3],
    /// The heading (radians, adapter space).
    pub facing: f32,
    /// The story the player had when the spot was saved (the progression blob, hex on disk).
    #[serde(with = "hex_bytes")]
    pub story: Vec<u8>,
    /// Whether a login may put the player back on this spot. `false` when the story had moved on to a
    /// place the spot does not lead to (FF7: a story whose entry field is elsewhere and was never
    /// reached), or when another account's story governed the player's place (a party member).
    pub resumable: bool,
    /// When this spot was saved (seconds since the epoch).
    pub updated_at: u64,
}

impl SavedLocation {
    /// A record to store (the store stamps the schema version).
    #[allow(clippy::too_many_arguments)] // One argument per field of a plain record.
    pub fn new(account_id: AccountId, game: GameId, area: impl Into<String>, pos: [f32; 3], facing: f32, story: Vec<u8>, resumable: bool, updated_at: u64) -> Self {
        Self { schema_version: SCHEMA_VERSION, account_id, game, area: area.into(), pos, facing, story, resumable, updated_at }
    }

    /// Why this record cannot be stored or used, if it cannot.
    fn invalid(&self) -> Option<String> {
        if self.area.is_empty() || self.area.len() > MAX_LOCATION_AREA_BYTES {
            return Some(format!("area key of {} bytes (1 to {MAX_LOCATION_AREA_BYTES})", self.area.len()));
        }
        // An area key is a field's name (FF7: `md8_4`; the fixture: `yard`), never a path.
        if !self.area.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')) {
            return Some("area key has characters an area key never has".to_string());
        }
        if !self.pos.iter().all(|v| v.is_finite()) || !self.facing.is_finite() {
            return Some("position or facing is not finite".to_string());
        }
        if self.story.len() > MAX_LOCATION_STORY_BYTES {
            return Some(format!("story copy of {} bytes (at most {MAX_LOCATION_STORY_BYTES})", self.story.len()));
        }
        None
    }
}

impl Store {
    /// `progression/<account>/<Game>.location.json`. A game key is letters, digits and `_`, so no key can
    /// spell the `.location` part: it never names a game's story or character record.
    fn location_path(&self, account: u64, game: GameId) -> Result<PathBuf> {
        let key = game_key(game);
        if !is_safe_key(&key) {
            return Err(PersistError::InvalidField { field: "game", reason: format!("{key:?} cannot be used as a file name") });
        }
        Ok(self.progression_dir(account).join(format!("{key}.location.json")))
    }

    /// Store (replacing) where the account's player stands in `location.game`. Nothing else is touched.
    /// Fails with [`PersistError::UnknownAccount`] for an account the index does not have (one deleted
    /// while this write was queued) and with [`PersistError::InvalidField`] for a record that could never
    /// be used (an empty or odd area key, a position that is not finite, an oversized story copy).
    pub fn put_location(&self, location: &SavedLocation) -> Result<()> {
        if let Some(reason) = location.invalid() {
            return Err(PersistError::InvalidField { field: "location", reason });
        }
        let path = self.location_path(location.account_id.0, location.game)?;
        let _counters = self.writer()?;
        if !self.index().contains_account(location.account_id.0) {
            return Err(PersistError::UnknownAccount(location.account_id));
        }
        fsio::ensure_dir(&self.progression_dir(location.account_id.0))?;
        let mut stored = location.clone();
        stored.schema_version = SCHEMA_VERSION;
        fsio::write_json(&path, &stored, MAX_LOCATION_RECORD_BYTES)
    }

    /// Where the account's player last stood in `game`: `None` if no spot was ever saved. A record that
    /// is damaged, from a newer build, for another account or game, or unusable is an error (the caller
    /// joins the player as if there were none, and says so); it is never replaced behind anybody's back.
    pub fn get_location(&self, account: AccountId, game: GameId) -> Result<Option<SavedLocation>> {
        if !self.index().contains_account(account.0) {
            return Err(PersistError::UnknownAccount(account));
        }
        let path = self.location_path(account.0, game)?;
        let Some(location) = fsio::read_json::<SavedLocation>(&path, MAX_LOCATION_RECORD_BYTES)? else {
            return Ok(None);
        };
        if location.account_id != account || location.game != game {
            return Err(PersistError::corrupt(&path, "the location record is for another account or game"));
        }
        if let Some(reason) = location.invalid() {
            return Err(PersistError::corrupt(&path, reason));
        }
        Ok(Some(location))
    }
}

impl Store {
    /// Remove every saved spot of `account` (all games), then its progression directory if that leaves it empty.
    /// Call it after [`Store::delete_account`]: a delete moves the account's records to the trash and a spot is not
    /// one of them (a restored character joins where everyone joins). Safe for an account the index no longer has;
    /// no spot is no error. Returns the removed files, relative to the store's root (`progression/<id>/<file>`).
    pub fn forget_locations(&self, account: AccountId) -> Result<Vec<String>> {
        let _counters = self.writer()?;
        let dir = self.progression_dir(account.0);
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(PersistError::io("list progression directory", &dir, e)),
        };
        let mut removed = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| PersistError::io("list progression directory", &dir, e))?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let is_spot = name.strip_suffix(".location.json").is_some_and(is_safe_key);
            if is_spot && entry.file_type().is_ok_and(|t| t.is_file()) {
                let path = entry.path();
                std::fs::remove_file(&path).map_err(|e| PersistError::io("remove saved spot", &path, e))?;
                removed.push(format!("{}/{}/{name}", super::PROGRESSION_DIR, account.0));
            }
        }
        // Empty now (the delete moved everything else), or it stays: anything else in it is not ours to remove.
        if !removed.is_empty() && std::fs::remove_dir(&dir).is_ok() {
            let _ = fsio::sync_dir(&self.root.join(super::PROGRESSION_DIR));
        }
        Ok(removed)
    }
}

/// The story copy as a hex string on disk (the same spelling as a progression record's blob).
mod hex_bytes {
    use serde::{de::Error, Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        let mut text = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            text.push_str(&format!("{b:02x}"));
        }
        s.serialize_str(&text)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(d)?;
        if text.len() % 2 != 0 || text.len() > 2 * super::MAX_LOCATION_STORY_BYTES {
            return Err(D::Error::custom("story copy is not an even, bounded hex string"));
        }
        (0..text.len())
            .step_by(2)
            .map(|i| text.get(i..i + 2).and_then(|pair| u8::from_str_radix(pair, 16).ok()).ok_or_else(|| D::Error::custom("story copy is not hex")))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmv_core::GameId;

    fn store_with_account(dir: &std::path::Path) -> (Store, AccountId) {
        let store = Store::open(dir).unwrap();
        let registered = store.register("Spot", [1, 2, 3], "lean", None, 0).unwrap();
        (store, registered.account_id)
    }

    fn spot(account: AccountId) -> SavedLocation {
        SavedLocation::new(account, GameId::Ff7, "md1_2", [120.5, -340.25, 17.0], 1.5, vec![1, 8, 0], true, 99)
    }

    #[test]
    fn a_saved_spot_comes_back_exactly_and_sits_beside_the_other_records() {
        let dir = tempfile::tempdir().unwrap();
        let (store, account) = store_with_account(dir.path());
        assert_eq!(store.get_location(account, GameId::Ff7).unwrap(), None, "nothing saved yet");
        store.put_location(&spot(account)).unwrap();
        assert_eq!(store.get_location(account, GameId::Ff7).unwrap(), Some(spot(account)));
        let file = dir.path().join("progression").join(account.0.to_string()).join("Ff7.location.json");
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("\"story\": \"010800\""), "the story copy is hex, as a progression blob: {text}");
        // A newer spot replaces the old one; the story record is not touched.
        let mut later = spot(account);
        later.area = "nrthmk".into();
        later.updated_at = 120;
        store.put_location(&later).unwrap();
        assert_eq!(store.get_location(account, GameId::Ff7).unwrap(), Some(later));
        assert!(store.get_progression(account, GameId::Ff7).unwrap().blob.is_empty(), "the story record is its own");
    }

    #[test]
    fn a_spot_for_a_deleted_or_unknown_account_is_refused_and_creates_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (store, account) = store_with_account(dir.path());
        let ghost = AccountId(account.0 + 40);
        assert!(matches!(store.put_location(&spot(ghost)), Err(PersistError::UnknownAccount(_))));
        assert!(!dir.path().join("progression").join(ghost.0.to_string()).exists(), "no directory comes back for an unknown account");
        assert!(matches!(store.get_location(ghost, GameId::Ff7), Err(PersistError::UnknownAccount(_))));
    }

    #[test]
    fn unusable_spots_are_never_stored_and_a_damaged_file_is_an_error_not_a_default() {
        let dir = tempfile::tempdir().unwrap();
        let (store, account) = store_with_account(dir.path());
        let bad = [
            SavedLocation { area: String::new(), ..spot(account) },
            SavedLocation { area: "x".repeat(MAX_LOCATION_AREA_BYTES + 1), ..spot(account) },
            SavedLocation { area: "../accounts".into(), ..spot(account) },
            SavedLocation { area: "md 1".into(), ..spot(account) },
            SavedLocation { pos: [f32::NAN, 0.0, 0.0], ..spot(account) },
            SavedLocation { facing: f32::INFINITY, ..spot(account) },
            SavedLocation { story: vec![0; MAX_LOCATION_STORY_BYTES + 1], ..spot(account) },
        ];
        for location in &bad {
            assert!(matches!(store.put_location(location), Err(PersistError::InvalidField { .. })), "{location:?}");
        }
        assert_eq!(store.get_location(account, GameId::Ff7).unwrap(), None, "none of them was written");
        let file = dir.path().join("progression").join(account.0.to_string()).join("Ff7.location.json");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        for text in ["{", r#"{"schema_version":1}"#, r#"{"schema_version":99,"area":"md1_1"}"#] {
            std::fs::write(&file, text).unwrap();
            assert!(store.get_location(account, GameId::Ff7).is_err(), "{text}");
            assert_eq!(std::fs::read_to_string(&file).unwrap(), text, "a damaged file is left as it is");
        }
        // A record that names another account is not this account's spot.
        let mut other = spot(account);
        other.account_id = AccountId(account.0 + 1);
        std::fs::write(&file, serde_json::to_string(&other).unwrap()).unwrap();
        assert!(store.get_location(account, GameId::Ff7).is_err());
    }

    #[test]
    fn a_deleted_character_leaves_no_spot_behind_once_its_spots_are_forgotten() {
        let dir = tempfile::tempdir().unwrap();
        let (store, account) = store_with_account(dir.path());
        store.put_progression(&crate::Progression::new(account, GameId::Ff7, vec![1, 8, 0], 5)).unwrap();
        store.put_location(&spot(account)).unwrap();
        store.delete_account(account, 50).unwrap();
        let progression = dir.path().join("progression").join(account.0.to_string());
        assert!(progression.join("Ff7.location.json").exists(), "a delete moves records, and a spot is not one of them");
        assert!(!progression.join("Ff7.json").exists(), "the story went to the trash");
        assert_eq!(store.forget_locations(account).unwrap(), [format!("progression/{}/Ff7.location.json", account.0)]);
        assert!(!progression.exists(), "nothing of the deleted account stays in the live directories");
        assert!(matches!(store.put_location(&spot(account)), Err(PersistError::UnknownAccount(_))), "and no late write brings it back");
        assert!(!progression.exists());
        assert!(store.forget_locations(account).unwrap().is_empty(), "forgetting twice is no error");
    }

    #[test]
    fn a_snapshot_carries_the_spot_and_a_store_reopened_on_it_reads_it() {
        let dir = tempfile::tempdir().unwrap();
        let (store, account) = store_with_account(&dir.path().join("live"));
        store.put_location(&spot(account)).unwrap();
        store.snapshot(dir.path().join("copy")).unwrap();
        drop(store);
        let copy = Store::open(dir.path().join("copy")).unwrap();
        assert_eq!(copy.get_location(account, GameId::Ff7).unwrap(), Some(spot(account)));
    }
}
