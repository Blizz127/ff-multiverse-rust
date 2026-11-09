//! The player's own original character on the wire (protocol v8): what a registration asks for
//! ([`NewCharacter`], in `Hello`) and what the server says it holds ([`CharacterCard`], in `Welcome`).
//!
//! Every player has ONE original character that travels across games. In a game it takes a canon party
//! member's COMBAT slot (FF7: Barret's, say) while that canon character stays in the story as a non-player
//! character; dialogue is never name-swapped; identity and appearance persist across games and each game draws
//! them its own way. Three things are therefore separate, and the protocol keeps them separate:
//!
//! * the **identity**: name, colour and a game-agnostic body choice. Same in every game;
//! * the **binding**: how the hosted game draws the character. It reaches the other players as the entity's
//!   `appearance_ref` ([`fmv_core::AvatarAppearance`]) and is independent of the slot: a player who holds
//!   Barret's slot is not drawn as Barret on the field;
//! * the **slot**: the canon party member whose combat template and experience curve the character fights
//!   with. It is what `BattleMember::character` and `CharacterProgress::character` carry. It is chosen once,
//!   at creation, and never changes (FF7's experience curves differ per character, so a slot change would
//!   change a player's level).
//!
//! Nothing here is game data: ids, a colour and a name.

use serde::{Deserialize, Serialize};

/// Longest name a sender puts in a [`CharacterCard`] or a `BattleMember` (characters): the display-name limit
/// (`fmv_core::AvatarAppearance::MAX_NAME`). A receiver bounds what it keeps by the same number.
pub const MAX_CHARACTER_NAME_CHARS: usize = fmv_core::AvatarAppearance::MAX_NAME;

/// Longest body or appearance id a sender puts in a [`NewCharacter`] or a [`CharacterCard`] (characters): the
/// limit of an `appearance_ref`.
pub const MAX_CHARACTER_REF_CHARS: usize = fmv_core::AvatarAppearance::MAX_APPEARANCE_REF;

/// What a registration asks for: the character to create. Only a registration uses it (`Auth::Register`); a
/// token login ignores it, except `tint`, which colours an account that has no avatar yet (one made by other
/// tooling). After the registration the server decides everything from the stored character.
///
/// Every choice is checked by the server against what the hosted game offers, and an unknown body is not an
/// error (the game's default body is used: an unknown choice must not cost anyone their registration). The slot
/// is different: it fixes how the character fights for good, so a slot the game does not offer to a new
/// character is refused (`Reject`, code `BadRequest`) instead of being replaced by another one. FF7 offers
/// the canon party members except Cloud, who stays the canon protagonist: ids 1 to 8. Two players may hold the
/// same slot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewCharacter {
    /// The identity colour. The client tints the placeholder model with it and the server shows it to everyone.
    pub tint: [u8; 3],
    /// The identity's body choice: an archetype id the hosted game offers (FF7: the generic placeholder
    /// bodies of the character creator). `None`, or one the game does not know: the game's default body.
    pub body: Option<String>,
    /// The canon combat slot: the game's own character id (FF7: Barret 1, Tifa 2, Aerith 3, Red XIII 4,
    /// Yuffie 5, Cait Sith 6, Vincent 7, Cid 8; Cloud, 0, is not offered to new characters). `None`: the
    /// server's default for the game (FF7: Barret). Ignored by a game that has no combat slots.
    pub slot: Option<u8>,
}

impl NewCharacter {
    /// A request with this colour and no choices (the server's defaults for the rest).
    pub fn with_tint(tint: [u8; 3]) -> Self {
        Self { tint, body: None, slot: None }
    }
}

/// What the server holds of the player's own character, told once in `Welcome`. It is what a client shows
/// ("who am I and what do I fight as") and what a smoke test compares across a server restart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CharacterCard {
    /// The character's name (the account's display name).
    pub name: String,
    /// The identity colour.
    pub tint: [u8; 3],
    /// The identity's body choice. `None`: never chosen (an account that predates the choice).
    pub body: Option<String>,
    /// How the hosted game draws the character: the binding, as the entity snapshots of the area say it
    /// (`appearance_ref`). `None`: the game's default look.
    pub binding: Option<String>,
    /// The canon combat slot the character fills in the hosted game (FF7: the character id that
    /// `CharacterProgress` and `BattleMember` carry). `None`: the game has no combat slots.
    pub slot: Option<u8>,
    /// The character was made from an account that predates player characters (it keeps the look it had and
    /// the character it was fighting as); `false` for one made with the character creator.
    pub migrated: bool,
}

impl CharacterCard {
    /// A card with only an identity (what a server without a game's character data tells).
    pub fn plain(name: impl Into<String>, tint: [u8; 3]) -> Self {
        Self { name: name.into(), tint, body: None, binding: None, slot: None, migrated: false }
    }
}

/// `name` as a sender puts it on the wire: control characters dropped, at most
/// [`MAX_CHARACTER_NAME_CHARS`] characters.
pub fn clip_character_name(name: &str) -> String {
    name.chars().filter(|c| !c.is_control()).take(MAX_CHARACTER_NAME_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_limits_are_the_display_name_and_appearance_limits() {
        assert_eq!(MAX_CHARACTER_NAME_CHARS, 24);
        assert_eq!(MAX_CHARACTER_REF_CHARS, 64);
    }

    #[test]
    fn a_name_is_clipped_to_the_limit_without_control_characters() {
        assert_eq!(clip_character_name("Red XIII"), "Red XIII");
        assert_eq!(clip_character_name("a\u{1b}[31mb\n"), "a[31mb");
        assert_eq!(clip_character_name(&"x".repeat(100)).chars().count(), MAX_CHARACTER_NAME_CHARS);
        // Counted in characters, not bytes.
        let wide = "\u{10FFFF}".repeat(40);
        assert_eq!(clip_character_name(&wide).chars().count(), MAX_CHARACTER_NAME_CHARS);
    }

    #[test]
    fn constructors_leave_the_choices_to_the_server() {
        assert_eq!(NewCharacter::with_tint([1, 2, 3]), NewCharacter { tint: [1, 2, 3], body: None, slot: None });
        let card = CharacterCard::plain("Cid", [4, 5, 6]);
        assert_eq!((card.body, card.binding, card.slot, card.migrated), (None, None, None, false));
    }
}
