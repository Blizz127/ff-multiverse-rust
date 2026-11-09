//! The hosted game's rules for a player's original character (protocol v8).
//!
//! `fmv_persist::PlayerCharacter` is game-agnostic: it keeps an identity and, per game, a binding, a slot and
//! pointers, and checks only what is true of every game. **What a game offers** is decided here: which bodies and
//! combat slots a NEW character may have, what an account that predates the record becomes, and what is shown of a
//! character to the others and to its player.
//!
//! # FF7
//!
//! * **Bodies** are the generic, non-canon placeholder models of the owner's own `char.lgp`
//!   ([`fmv_ff7::playable::PLACEHOLDER_BODIES`]), chosen by an archetype id; an unknown or missing choice is the
//!   default body (an unknown choice must not cost anyone their registration). The binding is the body's model
//!   (`ff7/body/<archetype>`), tinted by the identity on the client.
//! * **Slots** are the canon party members except Cloud: ids 1 to 8 (Barret, Tifa, Aerith, Red XIII, Yuffie, Cait
//!   Sith, Vincent, Cid). Cloud stays the canon protagonist, so slot 0 is refused to a new character (and so is
//!   anything above 8); a registration that names no slot gets [`FF7_DEFAULT_SLOT`]. Two players may hold the same
//!   slot. The slot is fixed at creation.
//! * **A migrated account** (one that predates the record) keeps what it had, so that none of its numbers moves: its
//!   slot is the character the builds before v8 fought it as (the character its stored FF7 look names, Cloud
//!   without one: slot 0 is allowed here), its binding is the canon look it chose, or, if it never chose one (the
//!   old default stood in for it), the default body.
//! * **A v7 compatibility mirror**: a new character's avatar profile carries the canon look of its slot
//!   ([`Creation::v7_look`]), so that a server rolled back to v7, which fights an account as the character its look
//!   names, still fights the character as its slot. Nothing in v8 reads it; the binding and the slot of the record
//!   are the truth. (Cait Sith has no canon look: a rolled-back v7 fights that slot as Cloud.)
//!
//! Every other game (the synthetic FIXTURE) has no bodies and no slots: nothing is stored for them but the
//! pointers, and a choice the client sent is ignored.

use fmv_core::{AvatarAppearance, GameId};
use fmv_persist::{CharacterOrigin, GameCharacter, LegacyView, PlayerCharacter};
use fmv_protocol::{CharacterCard, NewCharacter};

use crate::progress;

/// The FF7 slot of a new character that names none: Barret. (Every FF7 character gets the same starter
/// grimoire, and Barret has a limit break, so a character that never chose still plays the whole battle UI.)
pub const FF7_DEFAULT_SLOT: u8 = 1;
/// The last FF7 character id (Cid).
pub const FF7_LAST_SLOT: u8 = 8;

/// Why a new character's choices were refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The slot is not one the game offers to a new character (FF7: Cloud's, or no character at all).
    SlotNotOffered,
}

impl Refusal {
    /// The sentence for the player (sent in a `Reject`): never echoes what the client sent.
    pub fn client_text(self) -> &'static str {
        match self {
            Refusal::SlotNotOffered => {
                "that combat slot is not available to a new character (the protagonist's slot stays his); choose another party member"
            }
        }
    }
}

/// What a registration becomes: the identity body choice, the game's entry of the character and the mirror for
/// a rolled-back server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Creation {
    /// The identity's body choice as stored (an archetype id the game offers; empty where the game has none).
    pub body: String,
    /// The character's entry for the hosted game: binding, slot, pointers.
    pub entry: GameCharacter,
    /// The canon look to put in the avatar profile's per-game look for a v7 server (see the module docs).
    pub v7_look: Option<&'static str>,
}

/// Decide what a new character with these choices is in `game`, or why it cannot be.
pub fn plan_creation(game: GameId, requested: &NewCharacter) -> Result<Creation, Refusal> {
    match game {
        GameId::Ff7 => {
            let slot = match requested.slot {
                None => FF7_DEFAULT_SLOT,
                Some(slot) if (1..=FF7_LAST_SLOT).contains(&slot) => slot,
                Some(_) => return Err(Refusal::SlotNotOffered),
            };
            let body = fmv_ff7::playable::body_or_default(requested.body.as_deref());
            Ok(Creation {
                body: body.archetype.to_owned(),
                entry: GameCharacter {
                    binding: Some(body.model.appearance_ref.to_owned()),
                    slot: Some(slot),
                    progress: GameCharacter::pointers(game, Some(slot)),
                },
                v7_look: progress::look_of_character(slot),
            })
        }
        // A game with no bodies and no slots keeps no choice.
        _ => Ok(Creation { body: String::new(), entry: GameCharacter { progress: GameCharacter::pointers(game, None), ..GameCharacter::default() }, v7_look: None }),
    }
}

/// The entry an account that predates the player-character record gets in `game` the first time it logs in (the
/// store calls this while it adds the record; see `fmv_persist::Store::ensure_player_character`). **Nothing the
/// account has is changed:** the slot is what the builds before v8 fought it as, so its numbers do not move.
pub fn legacy_entry(game: GameId, view: &LegacyView<'_>) -> GameCharacter {
    match game {
        GameId::Ff7 => {
            // What a v7 server showed and fought: the canon look the stored profile names (anything else was shown
            // as no look), and the character that look names (Cloud for none).
            let look = view
                .avatar
                .and_then(|avatar| avatar.override_for(game))
                .and_then(fmv_ff7::playable::resolve_appearance)
                .map(|model| model.appearance_ref);
            let slot = progress::character_for_look(game, look);
            // It keeps the canon look it chose. An account that never chose one was drawn as the default stand-in;
            // it never chose Cloud, so it is bound to the default body instead of staying a Cloud.
            let binding = look.map_or_else(|| fmv_ff7::playable::body_or_default(None).model.appearance_ref, |l| l).to_owned();
            GameCharacter { binding: Some(binding), slot: Some(slot), progress: GameCharacter::pointers(game, Some(slot)) }
        }
        _ => GameCharacter { progress: GameCharacter::pointers(game, None), ..GameCharacter::default() },
    }
}

/// The binding as the game itself spells it, or `None` for one the game does not know: what is stored is not
/// trusted (it reaches every other player), so a reference that is not a model the game offers (a hand-edited
/// file: a path, another game's id) is shown as no look.
pub fn canonical_binding(game: GameId, raw: &str) -> Option<String> {
    match game {
        GameId::Ff7 => fmv_ff7::playable::resolve_model(raw).map(|model| model.appearance_ref.to_owned()),
        _ => None,
    }
}

/// What the other players see of a character in `game`: its name and colour (identity) and its binding.
pub fn shown(game: GameId, character: &PlayerCharacter) -> AvatarAppearance {
    AvatarAppearance {
        display_name: character.identity.name.clone(),
        tint: character.identity.tint,
        appearance_ref: character.binding(game).and_then(|binding| canonical_binding(game, binding)),
    }
}

/// What the character's own player is told in `Welcome`.
pub fn card(game: GameId, character: &PlayerCharacter) -> CharacterCard {
    CharacterCard {
        name: character.identity.name.clone(),
        tint: character.identity.tint,
        body: Some(character.identity.body.clone()).filter(|body| !body.is_empty()),
        binding: character.binding(game).and_then(|binding| canonical_binding(game, binding)),
        slot: character.slot(game),
        migrated: character.origin == CharacterOrigin::Migrated,
    }
}

/// `text` as one `key=value` token of a log line: letters, digits and `- . _ ~` kept, every other byte `%XX` (the
/// encoding of the client's `SMOKE name=`), so that "Red XIII" never splits the line the smoke scripts read.
pub fn log_token(text: &str) -> String {
    text.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') { char::from(b).to_string() } else { format!("%{b:02X}") })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmv_persist::{AvatarId, AvatarProfile};
    use std::collections::BTreeMap;

    fn request(body: Option<&str>, slot: Option<u8>) -> NewCharacter {
        NewCharacter { tint: [1, 2, 3], body: body.map(str::to_owned), slot }
    }

    #[test]
    fn a_new_ff7_character_takes_a_slot_from_one_to_eight_and_a_body_from_the_table() {
        for slot in 1..=FF7_LAST_SLOT {
            let creation = plan_creation(GameId::Ff7, &request(Some("slim"), Some(slot))).unwrap();
            assert_eq!(creation.entry.slot, Some(slot));
            assert_eq!(creation.entry.character_ref(), Some(&fmv_persist::ProgressRef::Character { game: GameId::Ff7, character: slot }));
            assert!(creation.entry.has_story_ref());
            assert_eq!(creation.body, "slim");
            assert_eq!(creation.entry.binding.as_deref(), Some("ff7/body/slim"));
            // The mirror lets a rolled-back v7 fight it as its slot (Cait Sith, 6, has no look).
            assert_eq!(creation.v7_look.map(|look| progress::character_for_look(GameId::Ff7, Some(look))), (slot != 6).then_some(slot));
        }
    }

    #[test]
    fn cloud_and_nobody_are_refused_to_a_new_character_and_none_is_barret() {
        for bad in [Some(0u8), Some(9), Some(200), Some(u8::MAX)] {
            assert_eq!(plan_creation(GameId::Ff7, &request(None, bad)), Err(Refusal::SlotNotOffered), "{bad:?}");
        }
        let default = plan_creation(GameId::Ff7, &request(None, None)).unwrap();
        assert_eq!(default.entry.slot, Some(FF7_DEFAULT_SLOT));
        assert_eq!(default.entry.slot, Some(1), "Barret");
        // The sentence says what to do and never echoes anything.
        assert!(Refusal::SlotNotOffered.client_text().contains("choose another"));
    }

    #[test]
    fn an_unknown_or_missing_body_is_the_default_body_and_never_an_error() {
        let default = fmv_ff7::playable::body_or_default(None);
        for raw in [None, Some(""), Some("nobody"), Some("../../etc/passwd"), Some("ff7/cloud"), Some("\u{1b}[31m")] {
            let creation = plan_creation(GameId::Ff7, &request(raw, Some(2))).unwrap();
            assert_eq!(creation.body, default.archetype, "{raw:?}");
            assert_eq!(creation.entry.binding.as_deref(), Some(default.model.appearance_ref));
        }
        // Spelling does not matter to the table.
        let slim = fmv_ff7::playable::body_or_default(Some("slim"));
        assert_eq!(plan_creation(GameId::Ff7, &request(Some(" SLIM "), Some(2))).unwrap().body, slim.archetype);
    }

    #[test]
    fn the_binding_is_independent_of_the_slot() {
        // The same body with every slot: the same look, whoever it fights as.
        let looks: Vec<_> = (1..=FF7_LAST_SLOT)
            .map(|slot| plan_creation(GameId::Ff7, &request(Some("slim"), Some(slot))).unwrap().entry.binding)
            .collect();
        assert!(looks.windows(2).all(|w| w[0] == w[1]), "{looks:?}");
        // And no slot's binding is a canon character's model: nobody is drawn as the canon character it fights as.
        for slot in 1..=FF7_LAST_SLOT {
            for body in fmv_ff7::playable::PLACEHOLDER_BODIES {
                let creation = plan_creation(GameId::Ff7, &request(Some(body.archetype), Some(slot))).unwrap();
                let binding = creation.entry.binding.unwrap();
                assert!(fmv_ff7::playable::resolve_appearance(&binding).is_none(), "{binding} is a canon model");
            }
        }
    }

    #[test]
    fn a_game_without_slots_keeps_no_choice_and_ignores_what_the_client_sent() {
        let creation = plan_creation(GameId::Fixture, &request(Some("slim"), Some(0))).unwrap();
        assert_eq!(creation.body, "");
        assert_eq!((creation.entry.binding, creation.entry.slot, creation.v7_look), (None, None, None));
        assert_eq!(creation.entry.progress, vec![fmv_persist::ProgressRef::Story { game: GameId::Fixture }]);
    }

    fn avatar(look: Option<&str>) -> AvatarProfile {
        AvatarProfile {
            schema_version: 1,
            avatar_id: AvatarId(1),
            account_id: fmv_core::AccountId(1),
            display_name: "Old".into(),
            tint: [9, 8, 7],
            base_archetype: "default".into(),
            per_game_overrides: look.map(|l| BTreeMap::from([("Ff7".to_owned(), l.to_owned())])).unwrap_or_default(),
            created_at: 0,
            updated_at: 0,
        }
    }

    fn migrated(look: Option<&str>) -> GameCharacter {
        legacy_entry(GameId::Ff7, &LegacyView { name: "Old", avatar: Some(&avatar(look)), progress: None })
    }

    #[test]
    fn an_account_that_predates_the_record_keeps_the_character_its_look_named_and_that_look() {
        for (look, slot) in [("ff7/cloud", 0u8), ("ff7/barret", 1), ("ff7/tifa", 2), ("ff7/aerith", 3), ("ff7/red_xiii", 4), ("ff7/yuffie", 5), ("ff7/vincent", 7), ("ff7/cid", 8)] {
            let entry = migrated(Some(look));
            assert_eq!(entry.slot, Some(slot), "{look}");
            assert_eq!(entry.binding.as_deref(), Some(look), "it keeps the look it chose");
            assert_eq!(entry.character_ref(), Some(&fmv_persist::ProgressRef::Character { game: GameId::Ff7, character: slot }));
        }
        // The look is read the way v7 read it: case and padding do not matter, and the canonical spelling is kept.
        assert_eq!(migrated(Some(" FF7/Tifa ")).binding.as_deref(), Some("ff7/tifa"));
        assert_eq!(migrated(Some(" FF7/Tifa ")).slot, Some(2));
    }

    #[test]
    fn an_account_without_a_usable_look_is_cloud_and_bound_to_the_default_body() {
        let default_body = fmv_ff7::playable::body_or_default(None).model.appearance_ref;
        for look in [None, Some("nobody"), Some("../../etc/passwd"), Some("ff7/body/slim"), Some("")] {
            let entry = migrated(look);
            assert_eq!(entry.slot, Some(0), "{look:?}: what v7 fought it as, and slot 0 is allowed here");
            assert_eq!(entry.binding.as_deref(), Some(default_body), "{look:?}: it never chose Cloud, so it is not drawn as Cloud");
        }
        // No avatar profile at all (an account made by other tooling): the same.
        let none = legacy_entry(GameId::Ff7, &LegacyView { name: "Old", avatar: None, progress: None });
        assert_eq!(none.slot, Some(0));
        // A game without slots: pointers to the story only.
        let fixture = legacy_entry(GameId::Fixture, &LegacyView { name: "Old", avatar: Some(&avatar(Some("ff7/tifa"))), progress: None });
        assert_eq!((fixture.slot, fixture.binding), (None, None));
    }

    #[test]
    fn what_is_stored_is_not_trusted_on_its_way_to_the_other_players() {
        let mut record = PlayerCharacter {
            schema_version: 1,
            account_id: fmv_core::AccountId(1),
            identity: fmv_persist::Identity { name: "Wedge".into(), tint: [1, 2, 3], body: "slim".into() },
            games: BTreeMap::from([("Ff7".to_owned(), plan_creation(GameId::Ff7, &request(Some("slim"), Some(3))).unwrap().entry)]),
            origin: CharacterOrigin::Created,
            created_at: 0,
            updated_at: 0,
        };
        let shown_look = shown(GameId::Ff7, &record);
        assert_eq!((shown_look.display_name.as_str(), shown_look.tint, shown_look.appearance_ref.as_deref()), ("Wedge", [1, 2, 3], Some("ff7/body/slim")));
        let card_now = card(GameId::Ff7, &record);
        assert_eq!((card_now.body.as_deref(), card_now.binding.as_deref(), card_now.slot, card_now.migrated), (Some("slim"), Some("ff7/body/slim"), Some(3), false));
        // A binding the game does not know (a hand edit) is shown as none, in the card as in the snapshot.
        record.games.get_mut("Ff7").unwrap().binding = Some("../../etc/passwd".into());
        assert_eq!(shown(GameId::Ff7, &record).appearance_ref, None);
        assert_eq!(card(GameId::Ff7, &record).binding, None);
        // Another game has no entry here: nothing is shown for it, and the card has no slot.
        assert_eq!(shown(GameId::Fixture, &record).appearance_ref, None);
        assert_eq!(card(GameId::Fixture, &record).slot, None);
        // A migrated record says so, and a legacy canon look is a binding too.
        record.origin = CharacterOrigin::Migrated;
        record.games.get_mut("Ff7").unwrap().binding = Some("ff7/cloud".into());
        record.identity.body = String::new();
        let card_now = card(GameId::Ff7, &record);
        assert_eq!((card_now.body, card_now.binding.as_deref(), card_now.migrated), (None, Some("ff7/cloud"), true));
    }

    #[test]
    fn a_log_token_never_splits_a_line_and_encodes_like_the_clients_name() {
        assert_eq!(log_token("Blizz"), "Blizz");
        assert_eq!(log_token("Red XIII"), "Red%20XIII");
        assert_eq!(log_token("a=b c\n"), "a%3Db%20c%0A");
        assert_eq!(log_token("\u{c4}rger"), "%C3%84rger");
        assert_eq!(log_token(""), "");
    }
}
