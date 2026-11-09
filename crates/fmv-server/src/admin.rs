//! The narrow public face of the hosted game's character rules, for operator tools (`fmv-characters`).
//!
//! The rules themselves stay in the private `character` module: what a game offers a character, what an account that
//! predates the player-character record becomes, which bindings the game knows. A binary under `src/bin/` is a
//! separate crate and cannot reach them, and `fmv_persist` (which keeps the records) cannot know them, so this module
//! hands the tool exactly three things and nothing else:
//!
//! * [`legacy_entry`]: the migration closure the server gives `Store::ensure_player_character` at a login, so an
//!   operator's tool that has to create the record of an account first builds it **exactly as that login would**;
//! * [`offered_bodies`] and [`body_named`]: the placeholder bodies the game offers (FF7: `fmv_ff7::playable`'s
//!   [`PLACEHOLDER_BODIES`](fmv_ff7::playable::PLACEHOLDER_BODIES)) and the lookup of one by its archetype id. The
//!   lookup **refuses** what is not offered (`None`), unlike a registration, which falls back to the default body so
//!   that an unknown choice never costs anyone an account: an operator who typed a canon character's name or a
//!   misspelling gets told, not a default;
//! * [`binding_kind`]: whether a stored binding is a canon character's look, a placeholder body, or something the game
//!   does not know (shown as no look), so that the tool can say what an account looks like.
//!
//! Nothing here reads a game file or a state directory.

use fmv_core::GameId;
use fmv_persist::{GameCharacter, LegacyView};

use crate::character;

/// The entry an account that predates the player-character record gets in `game` when its record is created: the
/// same function the server uses at a login (`character::legacy_entry`), so that an operator's tool and a login
/// build the same record from the same account. A test pins that they are one rule.
pub fn legacy_entry(game: GameId, view: &LegacyView<'_>) -> GameCharacter {
    character::legacy_entry(game, view)
}

/// A body a game offers to a character: a generic placeholder, never a canon character.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OfferedBody {
    /// The game-agnostic archetype id stored in the character's identity (FF7: `lean`, `sturdy`, `slight`).
    pub archetype: &'static str,
    /// The binding: the id of the model the game draws it with (FF7: `ff7/body/<archetype>`).
    pub binding: &'static str,
}

/// Every body `game` offers, the default first. Empty for a game that has no bodies (the FIXTURE and the games
/// that have no adapter yet).
pub fn offered_bodies(game: GameId) -> Vec<OfferedBody> {
    match game {
        GameId::Ff7 => fmv_ff7::playable::PLACEHOLDER_BODIES
            .iter()
            .map(|body| OfferedBody { archetype: body.archetype, binding: body.model.appearance_ref })
            .collect(),
        _ => Vec::new(),
    }
}

/// The body of `game` whose archetype id is `archetype` (ASCII case and surrounding whitespace do not matter; the
/// answer carries the canonical id), or `None` if the game offers no such body. A canon character's look
/// (`ff7/cloud`), its name (`cloud`), a model id (`ff7/body/lean`) and anything misspelt are not bodies.
pub fn body_named(game: GameId, archetype: &str) -> Option<OfferedBody> {
    match game {
        GameId::Ff7 => fmv_ff7::playable::resolve_body(archetype).map(|body| OfferedBody { archetype: body.archetype, binding: body.model.appearance_ref }),
        _ => None,
    }
}

/// What a stored binding is, in `game`'s terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingKind {
    /// A canon character's look (FF7: `ff7/cloud`, `ff7/tifa`, ...): what an account that predates the record had.
    CanonCharacter,
    /// One of the generic placeholder bodies ([`offered_bodies`]).
    PlaceholderBody,
    /// Not a model the game offers (a hand-edited file): the server shows such a binding as no look.
    Unknown,
}

/// What `binding` is in `game`.
pub fn binding_kind(game: GameId, binding: &str) -> BindingKind {
    match game {
        GameId::Ff7 => {
            if fmv_ff7::playable::resolve_appearance(binding).is_some() {
                BindingKind::CanonCharacter
            } else if fmv_ff7::playable::resolve_model(binding).is_some() {
                BindingKind::PlaceholderBody
            } else {
                BindingKind::Unknown
            }
        }
        _ => BindingKind::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmv_persist::{AvatarId, AvatarProfile};
    use std::collections::BTreeMap;

    fn avatar(look: Option<&str>) -> AvatarProfile {
        AvatarProfile {
            schema_version: 1,
            avatar_id: AvatarId(1),
            account_id: fmv_core::AccountId(1),
            display_name: "Blizz".into(),
            tint: [9, 8, 7],
            base_archetype: "default".into(),
            per_game_overrides: look.map(|l| BTreeMap::from([("Ff7".to_owned(), l.to_owned())])).unwrap_or_default(),
            created_at: 0,
            updated_at: 0,
        }
    }

    /// The tool must create a missing record the way a login does: the closure it hands the store is the server's own
    /// rule, whatever the account looks like and whatever the game.
    #[test]
    fn the_tools_migration_closure_is_exactly_the_servers() {
        let looks = [
            None,
            Some("ff7/cloud"),
            Some("ff7/barret"),
            Some("ff7/tifa"),
            Some(" FF7/Aerith "),
            Some("ff7/red_xiii"),
            Some("ff7/cid"),
            Some("ff7/body/slight"),
            Some("nobody"),
            Some(""),
            Some("../../etc/passwd"),
        ];
        for game in GameId::ALL {
            for look in looks {
                let profile = avatar(look);
                for avatar in [Some(&profile), None] {
                    let view = LegacyView { name: "Blizz", avatar, progress: None };
                    assert_eq!(legacy_entry(game, &view), character::legacy_entry(game, &view), "{game:?} {look:?} avatar={}", avatar.is_some());
                }
            }
        }
        // And it is the rule a migrated account is known by: its canon look is kept and names its slot.
        let profile = avatar(Some("ff7/cloud"));
        let entry = legacy_entry(GameId::Ff7, &LegacyView { name: "Blizz", avatar: Some(&profile), progress: None });
        assert_eq!((entry.slot, entry.binding.as_deref()), (Some(0), Some("ff7/cloud")));
    }

    #[test]
    fn every_offered_archetype_resolves_to_a_binding_the_game_accepts() {
        let offered = offered_bodies(GameId::Ff7);
        let archetypes: Vec<&str> = offered.iter().map(|b| b.archetype).collect();
        assert_eq!(archetypes, ["lean", "sturdy", "slight"], "the table, the default first");
        assert_eq!(archetypes.first().copied(), Some(fmv_ff7::playable::DEFAULT_BODY));
        for body in &offered {
            assert_eq!(body.binding, format!("ff7/body/{}", body.archetype));
            // The server's own filter on what reaches other players accepts it, spelled as it is stored.
            assert_eq!(character::canonical_binding(GameId::Ff7, body.binding).as_deref(), Some(body.binding), "{}", body.archetype);
            assert_eq!(binding_kind(GameId::Ff7, body.binding), BindingKind::PlaceholderBody);
            // The lookup finds it by its own id, and the placeholder is not a canon model.
            assert_eq!(body_named(GameId::Ff7, body.archetype), Some(*body));
            assert!(fmv_ff7::playable::resolve_appearance(body.binding).is_none(), "{} is a canon model", body.binding);
            // What a registration would store for the same choice is the same pair.
            let planned = character::plan_creation(GameId::Ff7, &fmv_protocol::NewCharacter { tint: [1, 2, 3], body: Some(body.archetype.to_owned()), slot: Some(1) }).unwrap();
            assert_eq!((planned.body.as_str(), planned.entry.binding.as_deref()), (body.archetype, Some(body.binding)));
        }
    }

    #[test]
    fn a_body_that_is_not_offered_is_none_and_a_canon_look_is_not_a_body() {
        for refused in ["cloud", "Cloud", "ff7/cloud", "ff7/tifa", "tifa", "ff7/body/lean", "nobody", "", " ", "../../etc/passwd", "lean sturdy", "\u{1b}[31m"] {
            assert_eq!(body_named(GameId::Ff7, refused), None, "{refused:?}");
        }
        // Spelling, case and padding do not matter, and the canonical id comes back.
        let slight = body_named(GameId::Ff7, " SLIGHT ").unwrap();
        assert_eq!((slight.archetype, slight.binding), ("slight", "ff7/body/slight"));
        // A game that has no bodies offers none, whatever is asked.
        for game in [GameId::Fixture, GameId::Ff8, GameId::Ff9, GameId::Ff10, GameId::Ff12] {
            assert!(offered_bodies(game).is_empty(), "{game:?}");
            assert_eq!(body_named(game, "lean"), None, "{game:?}");
            assert_eq!(binding_kind(game, "ff7/cloud"), BindingKind::Unknown, "{game:?}");
        }
    }

    /// What the tool writes is what the server then shows: after a restyle (with the tool's own closure and a body from
    /// the table) the character is drawn as the placeholder body and told to its player as such, in the colour it was
    /// given, with the slot it had and the flag of a migrated account; and the next login of the account finds the
    /// record as it is (no migration runs, nothing is written).
    #[test]
    fn a_restyled_record_is_shown_and_told_to_the_player_as_the_new_body_and_a_login_leaves_it_alone() {
        let dir = tempfile::tempdir().unwrap();
        let store = fmv_persist::Store::open(dir.path().join("state")).unwrap();
        let reg = store.register("Blizz", [0x5a, 0x7d, 0xf5], "default", Some((GameId::Ff7, "ff7/cloud")), 5).unwrap();
        let body = body_named(GameId::Ff7, "sturdy").unwrap();
        let change = fmv_persist::Restyle { body: body.archetype.to_owned(), binding: body.binding.to_owned(), tint: Some([255, 0, 0]) };
        let report = store.restyle_player_character(reg.account_id, GameId::Ff7, &change, 6, false, |view| legacy_entry(GameId::Ff7, view)).unwrap();
        let record = store.get_player_character(reg.account_id).unwrap().unwrap();
        assert_eq!(record, report.after);

        let shown = character::shown(GameId::Ff7, &record);
        assert_eq!((shown.display_name.as_str(), shown.tint, shown.appearance_ref.as_deref()), ("Blizz", [255, 0, 0], Some("ff7/body/sturdy")));
        let card = character::card(GameId::Ff7, &record);
        assert_eq!((card.body.as_deref(), card.binding.as_deref(), card.slot, card.migrated), (Some("sturdy"), Some("ff7/body/sturdy"), Some(0), true));
        // The canon look is gone from what anybody sees, and the slot (who it fights as) is still Cloud's.
        assert_eq!(binding_kind(GameId::Ff7, card.binding.as_deref().unwrap()), BindingKind::PlaceholderBody);

        let (again, added) = store.ensure_player_character(reg.account_id, GameId::Ff7, 9, |view| character::legacy_entry(GameId::Ff7, view)).unwrap();
        assert!(!added);
        assert_eq!(again, record);
    }

    #[test]
    fn a_binding_is_a_canon_look_a_placeholder_or_unknown() {
        for canon in ["ff7/cloud", "ff7/barret", "ff7/tifa", "ff7/aerith", "ff7/red_xiii", "ff7/yuffie", "ff7/vincent", "ff7/cid", " FF7/Tifa "] {
            assert_eq!(binding_kind(GameId::Ff7, canon), BindingKind::CanonCharacter, "{canon}");
        }
        for unknown in ["", "lean", "ff7/nobody", "../../etc/passwd", "ff8/squall"] {
            assert_eq!(binding_kind(GameId::Ff7, unknown), BindingKind::Unknown, "{unknown:?}");
        }
    }
}
