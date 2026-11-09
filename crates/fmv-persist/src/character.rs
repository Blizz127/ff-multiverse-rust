//! The player-character record (protocol v8): who a player's ONE original character is, how each game
//! draws it, which canon combat slot it fills there and where its progress lives.
//!
//! ```text
//! <root>/characters/<account_id>.json     PlayerCharacter (one per account for now)
//! ```
//!
//! # What it holds, and what it deliberately does not
//!
//! * **Identity** ([`Identity`]): the name, the colour and a game-agnostic body choice. They are the
//!   same in every game. (The account's `AvatarProfile` keeps a copy of name, colour and archetype made
//!   at creation: it is what a v7 build reads. Nothing changes them afterwards, so the copies cannot
//!   drift; a v8 build reads this record.)
//! * **Per-game state** ([`GameCharacter`], keyed by [`game_key`]):
//!   * the **binding**: how that game draws the character (an adapter-defined appearance reference; FF7:
//!     a generic placeholder body, or, for an account that predates this record, the canon look it had).
//!     It is stored per game and is independent of the slot;
//!   * the **slot**: the canon combat slot the character fills in that game (FF7: a canon party member's
//!     character id, whose combat template and experience curve the character fights with). It is fixed
//!     at creation: FF7's experience curves differ per character, so a slot change would change a player's
//!     level. The canon character stays in the story as an NPC;
//!   * the **progress pointers** ([`ProgressRef`]): where the character's progress for that game lives.
//!     **Pointers, never copies, and nothing is moved or re-keyed**: level, experience, AP, gil, the limit
//!     gauge and the inventory stay in the character-progress record the store has always filed under
//!     (account, game) (`progression/<account>/<Game>.character.json`), the story in the `Progression`
//!     blob of (account, game). A build from before this record finds them exactly where it left them.
//! * **No numbers.** Nothing in this record is a level, an experience point, an item or a story moment.
//!
//! # Compatibility
//!
//! The file is new: a build that does not know it never reads or writes it, and this build never edits a
//! file an older build wrote (records of v6 and v7 shape are migrated by *adding* this one, see
//! [`Store::ensure_player_character`](crate::Store::ensure_player_character)). Every field but the
//! account and the identity has a serde default, so a record that lacks one (written by an earlier shape
//! of this very record) reads with that default instead of being refused.
//!
//! The store is game-agnostic: which bodies, slots and appearance references are valid is the game's
//! adapter's business (the server checks them before it asks the store to keep anything). The store
//! checks only what is true in every game: sizes, characters, that the keys are known games, that a
//! pointer names the game it sits under and that the pointer's character is the slot.

use std::collections::BTreeMap;

use fmv_core::{AccountId, AvatarAppearance, GameId};
use serde::{Deserialize, Serialize};

use crate::error::{PersistError, Result};
use crate::model::{
    check_new_appearance_ref, game_key, parse_game_key, AvatarProfile, CharacterProgress, MAX_ARCHETYPE_BYTES,
};

/// Largest accepted player-character record file, on read and on write. A record is a few hundred
/// bytes (one entry per game, six games at most); this is the cap a corrupt or hostile file is held to.
pub const MAX_PLAYER_CHARACTER_BYTES: u64 = 16 * 1024;

/// Most pointers one game entry holds (one per kind of record: see [`ProgressRef`]).
const MAX_PROGRESS_REFS: usize = 4;

/// Who a character is, in every game.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    /// The character's name: the account's display name, as the server cleaned it.
    pub name: String,
    /// The colour the character is drawn in (a game applies it its own way).
    pub tint: [u8; 3],
    /// The game-agnostic body choice the player made (an archetype id; each adapter maps it to the closest
    /// model it has). Empty: none was ever chosen (an account that predates the choice).
    #[serde(default)]
    pub body: String,
}

/// How a record came to exist.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CharacterOrigin {
    /// Made by the character creator, with the player's own choices. What a record that does not say reads as.
    #[default]
    Created,
    /// Made at the first login of an account that predates player-character records, from its avatar profile
    /// and its character-progress record (which were not changed).
    Migrated,
}

/// Where a game's progress for the character lives. A pointer names a record the store already files
/// under (the record's account, `game`); it never holds the record's content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case")]
pub enum ProgressRef {
    /// The character-progress record of (the account, `game`): level, experience, AP, gil, the limit gauge
    /// and the inventory. `character` is the game's character id that record holds when it is this
    /// character's: **the slot** (so progress lives under (account, slot character), as it always has).
    Character { game: GameId, character: u8 },
    /// The `Progression` blob of (the account, `game`): the character's story progress, also its story phase.
    Story { game: GameId },
}

impl ProgressRef {
    /// The game this pointer is about.
    pub fn game(&self) -> GameId {
        match self {
            ProgressRef::Character { game, .. } | ProgressRef::Story { game } => *game,
        }
    }
}

/// What one game knows about a character.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameCharacter {
    /// How this game draws the character: an adapter-defined appearance reference. Independent of the slot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding: Option<String>,
    /// The canon combat slot the character fills in this game (opaque here). Fixed at creation. `None`: the
    /// game has no combat slots, or none was given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<u8>,
    /// Where this character's progress in this game lives.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub progress: Vec<ProgressRef>,
}

impl GameCharacter {
    /// The pointer to the character-progress record, if there is one.
    pub fn character_ref(&self) -> Option<&ProgressRef> {
        self.progress.iter().find(|r| matches!(r, ProgressRef::Character { .. }))
    }

    /// Does the entry point at the story blob?
    pub fn has_story_ref(&self) -> bool {
        self.progress.iter().any(|r| matches!(r, ProgressRef::Story { .. }))
    }

    /// The pointers a game with `slot` has: the character-progress record (when there is a slot) and the story.
    pub fn pointers(game: GameId, slot: Option<u8>) -> Vec<ProgressRef> {
        let mut refs = Vec::new();
        if let Some(character) = slot {
            refs.push(ProgressRef::Character { game, character });
        }
        refs.push(ProgressRef::Story { game });
        refs
    }

    fn validate(&self, game: GameId) -> Result<()> {
        if let Some(binding) = &self.binding {
            // The rule for a new per-game appearance reference (64 characters at most, no control characters),
            // named for this field: only this build writes bindings, so the older byte rule has no use here.
            check_new_appearance_ref(binding).map_err(|e| match e {
                PersistError::InvalidField { reason, .. } => PersistError::InvalidField { field: "binding", reason },
                other => other,
            })?;
        }
        if self.progress.len() > MAX_PROGRESS_REFS {
            return Err(PersistError::InvalidField {
                field: "progress",
                reason: format!("has {} pointers, over the {MAX_PROGRESS_REFS} limit", self.progress.len()),
            });
        }
        let (mut seen_character, mut seen_story) = (false, false);
        for pointer in &self.progress {
            if pointer.game() != game {
                return Err(PersistError::InvalidField {
                    field: "progress",
                    reason: format!("a pointer to {} sits under {}", game_key(pointer.game()), game_key(game)),
                });
            }
            let seen = match pointer {
                ProgressRef::Character { .. } => &mut seen_character,
                ProgressRef::Story { .. } => &mut seen_story,
            };
            if std::mem::replace(seen, true) {
                return Err(PersistError::InvalidField {
                    field: "progress",
                    reason: "has two pointers to the same kind of record".into(),
                });
            }
            if let ProgressRef::Character { character, .. } = pointer {
                if self.slot != Some(*character) {
                    return Err(PersistError::InvalidField {
                        field: "progress",
                        reason: format!(
                            "the character-progress pointer names character {character} but the slot is {:?}: \
                             progress lives under the slot's character",
                            self.slot
                        ),
                    });
                }
            }
        }
        Ok(())
    }
}

/// A player's persistent original character: `characters/<account_id>.json`. See the module documentation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayerCharacter {
    /// Record format version. The store stamps [`SCHEMA_VERSION`](crate::SCHEMA_VERSION) on every write.
    pub schema_version: u32,
    pub account_id: AccountId,
    pub identity: Identity,
    /// Per-game state by [`game_key`].
    #[serde(default)]
    pub games: BTreeMap<String, GameCharacter>,
    #[serde(default)]
    pub origin: CharacterOrigin,
    #[serde(default)]
    pub created_at: u64,
    #[serde(default)]
    pub updated_at: u64,
}

impl PlayerCharacter {
    /// What this character is in `game`, if the record has an entry for it.
    pub fn game(&self, game: GameId) -> Option<&GameCharacter> {
        self.games.get(&game_key(game))
    }

    /// The canon combat slot in `game`.
    pub fn slot(&self, game: GameId) -> Option<u8> {
        self.game(game).and_then(|g| g.slot)
    }

    /// The appearance binding in `game`.
    pub fn binding(&self, game: GameId) -> Option<&str> {
        self.game(game).and_then(|g| g.binding.as_deref())
    }

    /// Checks that are true in every game (sizes, characters, known games, pointer consistency). The
    /// account's name is compared by the store.
    pub(crate) fn validate(&self) -> Result<()> {
        let name_chars = self.identity.name.chars().count();
        if self.identity.name.is_empty() || name_chars > AvatarAppearance::MAX_NAME || self.identity.name.chars().any(char::is_control) {
            return Err(PersistError::InvalidField {
                field: "identity.name",
                reason: "must be 1 to 24 characters without control characters".into(),
            });
        }
        if self.identity.body.len() > MAX_ARCHETYPE_BYTES || self.identity.body.chars().any(char::is_control) {
            return Err(PersistError::InvalidField {
                field: "identity.body",
                reason: format!("must be at most {MAX_ARCHETYPE_BYTES} bytes without control characters"),
            });
        }
        if self.games.len() > GameId::ALL.len() {
            return Err(PersistError::InvalidField { field: "games", reason: "has more entries than there are games".into() });
        }
        for (key, entry) in &self.games {
            let Some(game) = parse_game_key(key) else {
                return Err(PersistError::InvalidField { field: "games", reason: format!("{key:?} is not a known game") });
            };
            entry.validate(game)?;
        }
        Ok(())
    }
}

/// What a caller asks for when it creates a character together with its account
/// ([`Store::register_with_character`](crate::Store::register_with_character)): the body choice and the
/// per-game state. The name and colour are the registration's own.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewPlayerCharacter {
    pub body: String,
    pub games: BTreeMap<String, GameCharacter>,
}

impl NewPlayerCharacter {
    /// A character with `entry` for `game` (and the body choice `body`).
    pub fn for_game(body: impl Into<String>, game: GameId, entry: GameCharacter) -> Self {
        Self { body: body.into(), games: BTreeMap::from([(game_key(game), entry)]) }
    }
}

/// What the migration closure of [`Store::ensure_player_character`](crate::Store::ensure_player_character)
/// is shown: the records an account that predates player-character records has, for the game being
/// entered. Nothing in it is written back.
#[derive(Debug, Clone, Copy)]
pub struct LegacyView<'a> {
    /// The account's display name.
    pub name: &'a str,
    /// Its avatar profile (name, colour, archetype and, per game, the look it chose).
    pub avatar: Option<&'a AvatarProfile>,
    /// Its character-progress record for the game.
    pub progress: Option<&'a CharacterProgress>,
}

/// The character-progress record a [`PlayerCharacter`] points at, and whether it is the character's own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedProgress {
    pub record: CharacterProgress,
    /// The character id the pointer names (the slot).
    pub expected_character: u8,
}

impl LinkedProgress {
    /// The record holds the slot's character. It does not when a build that derives the character from the
    /// look (v7) saved it after a rollback: the slot wins, and the next save writes it back.
    pub fn in_sync(&self) -> bool {
        self.record.character == self.expected_character
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ff7(slot: Option<u8>) -> GameCharacter {
        GameCharacter { binding: Some("ff7/body/sturdy".into()), slot, progress: GameCharacter::pointers(GameId::Ff7, slot) }
    }

    fn record(entry: GameCharacter) -> PlayerCharacter {
        PlayerCharacter {
            schema_version: 1,
            account_id: AccountId(2),
            identity: Identity { name: "Wedge".into(), tint: [1, 2, 3], body: "sturdy".into() },
            games: BTreeMap::from([("Ff7".to_owned(), entry)]),
            origin: CharacterOrigin::Created,
            created_at: 5,
            updated_at: 6,
        }
    }

    #[test]
    fn the_record_has_one_stable_json_shape() {
        let json = serde_json::to_value(record(ff7(Some(1)))).unwrap();
        assert_eq!(
            json,
            json!({
                "schema_version": 1,
                "account_id": 2,
                "identity": {"name": "Wedge", "tint": [1, 2, 3], "body": "sturdy"},
                "games": {"Ff7": {
                    "binding": "ff7/body/sturdy",
                    "slot": 1,
                    "progress": [
                        {"record": "character", "game": "Ff7", "character": 1},
                        {"record": "story", "game": "Ff7"}
                    ]
                }},
                "origin": "created",
                "created_at": 5,
                "updated_at": 6
            })
        );
        assert_eq!(serde_json::from_value::<PlayerCharacter>(json).unwrap(), record(ff7(Some(1))));
    }

    #[test]
    fn every_field_but_the_account_and_the_identity_has_a_default() {
        // The smallest record that reads: nothing else is required, so a record of an earlier shape of this
        // very file (or one an operator trimmed by hand) is read, not refused.
        let minimal = json!({
            "schema_version": 1,
            "account_id": 9,
            "identity": {"name": "A", "tint": [0, 0, 0]}
        });
        let read: PlayerCharacter = serde_json::from_value(minimal).unwrap();
        assert_eq!(read.identity.body, "");
        assert!(read.games.is_empty());
        assert_eq!(read.origin, CharacterOrigin::Created);
        assert_eq!((read.created_at, read.updated_at), (0, 0));
        assert!(read.validate().is_ok());
        // An entry's own fields are all optional too.
        let bare: GameCharacter = serde_json::from_value(json!({})).unwrap();
        assert_eq!(bare, GameCharacter::default());
        assert_eq!(serde_json::to_value(&bare).unwrap(), json!({}), "an empty entry writes nothing");
        // The account and the identity are what a record is about: without them it is not one.
        for missing in ["account_id", "identity"] {
            let mut doc = json!({"schema_version": 1, "account_id": 9, "identity": {"name": "A", "tint": [0, 0, 0]}});
            doc.as_object_mut().unwrap().remove(missing);
            assert!(serde_json::from_value::<PlayerCharacter>(doc).is_err(), "{missing}");
        }
    }

    #[test]
    fn a_slot_has_a_pointer_to_its_progress_record_and_the_story() {
        let entry = ff7(Some(3));
        assert_eq!(entry.character_ref(), Some(&ProgressRef::Character { game: GameId::Ff7, character: 3 }));
        assert!(entry.has_story_ref());
        // A game without slots points only at the story.
        let none = GameCharacter { progress: GameCharacter::pointers(GameId::Fixture, None), ..GameCharacter::default() };
        assert_eq!(none.character_ref(), None);
        assert_eq!(none.progress, vec![ProgressRef::Story { game: GameId::Fixture }]);
    }

    #[test]
    fn what_is_true_in_every_game_is_checked_and_nothing_game_specific_is() {
        assert!(record(ff7(Some(1))).validate().is_ok());
        // The store has no opinion on slots (the game's own rule: Cloud is refused for NEW FF7 characters,
        // and a migrated account may hold slot 0) or on which references exist.
        for slot in [0u8, 1, 8, 200, u8::MAX] {
            assert!(record(ff7(Some(slot))).validate().is_ok(), "{slot}");
        }
        let mut odd = record(ff7(Some(1)));
        odd.games.get_mut("Ff7").unwrap().binding = Some("whatever/the-adapter-says".into());
        assert!(odd.validate().is_ok());

        let reason = |pc: PlayerCharacter| match pc.validate() {
            Err(PersistError::InvalidField { field, reason }) => format!("{field}: {reason}"),
            other => panic!("expected a refusal, got {other:?}"),
        };
        let mut bad = record(ff7(Some(1)));
        bad.identity.name = String::new();
        assert!(reason(bad).starts_with("identity.name"));
        let mut bad = record(ff7(Some(1)));
        bad.identity.name = "x".repeat(25);
        assert!(reason(bad).starts_with("identity.name"));
        let mut bad = record(ff7(Some(1)));
        bad.identity.name = "a\nb".into();
        assert!(reason(bad).starts_with("identity.name"));
        let mut bad = record(ff7(Some(1)));
        bad.identity.body = "b\u{7}".into();
        assert!(reason(bad).starts_with("identity.body"));
        let mut bad = record(ff7(Some(1)));
        bad.identity.body = "b".repeat(MAX_ARCHETYPE_BYTES + 1);
        assert!(reason(bad).starts_with("identity.body"));
        let mut bad = record(ff7(Some(1)));
        bad.games.insert("Ff99".into(), GameCharacter::default());
        assert!(reason(bad).contains("not a known game"));
        let mut bad = record(ff7(Some(1)));
        bad.games.get_mut("Ff7").unwrap().binding = Some("x".repeat(65));
        assert!(reason(bad).contains("character limit"));
        let mut bad = record(ff7(Some(1)));
        bad.games.get_mut("Ff7").unwrap().binding = Some("\u{1b}[31m".into());
        assert!(reason(bad).contains("control"));
    }

    #[test]
    fn pointers_must_name_their_game_appear_once_and_follow_the_slot() {
        let reason = |entry: GameCharacter| match record(entry).validate() {
            Err(PersistError::InvalidField { field: "progress", reason }) => reason,
            other => panic!("expected a refused pointer, got {other:?}"),
        };
        // A pointer under the wrong game.
        let mut wrong = ff7(Some(1));
        wrong.progress = vec![ProgressRef::Story { game: GameId::Ff8 }];
        assert!(reason(wrong).contains("sits under Ff7"));
        // Two pointers to one kind of record.
        let mut twice = ff7(Some(1));
        twice.progress.push(ProgressRef::Story { game: GameId::Ff7 });
        assert!(reason(twice).contains("two pointers"));
        // The character pointer names the slot's character (progress lives under it): another one, or none, is not
        // this character's record.
        let mut moved = ff7(Some(1));
        moved.progress[0] = ProgressRef::Character { game: GameId::Ff7, character: 2 };
        assert!(reason(moved).contains("progress lives under the slot's character"));
        let mut slotless = ff7(None);
        slotless.progress = vec![ProgressRef::Character { game: GameId::Ff7, character: 1 }];
        assert!(reason(slotless).contains("slot is None"));
        // Too many.
        let mut many = ff7(Some(1));
        many.progress = (0..=MAX_PROGRESS_REFS).map(|_| ProgressRef::Story { game: GameId::Ff7 }).collect();
        assert!(reason(many).contains("limit"));
    }

    #[test]
    fn the_largest_valid_record_fits_the_record_cap_with_room_to_spare() {
        // Every game, the widest names and references, every pointer.
        let mut widest = record(ff7(Some(u8::MAX)));
        widest.schema_version = u32::MAX;
        widest.account_id = AccountId(u64::MAX);
        widest.identity.name = "\u{10FFFF}".repeat(AvatarAppearance::MAX_NAME);
        widest.identity.body = "b".repeat(MAX_ARCHETYPE_BYTES);
        widest.created_at = u64::MAX;
        widest.updated_at = u64::MAX;
        widest.games.clear();
        for game in GameId::ALL {
            let entry = GameCharacter {
                binding: Some("\u{10FFFF}".repeat(AvatarAppearance::MAX_APPEARANCE_REF)),
                slot: Some(u8::MAX),
                progress: GameCharacter::pointers(game, Some(u8::MAX)),
            };
            widest.games.insert(game_key(game), entry);
        }
        assert!(widest.validate().is_ok());
        let bytes = serde_json::to_vec_pretty(&widest).unwrap().len() as u64 + 1;
        assert!(bytes * 3 / 2 < MAX_PLAYER_CHARACTER_BYTES, "{bytes} bytes is too close to the cap of {MAX_PLAYER_CHARACTER_BYTES}");
    }
}
