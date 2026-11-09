//! The menu sound effects, and which key press makes which.
//!
//! The game has four: the cursor's click, the confirm, the cancel and the buzzer of a choice that
//! cannot be made. A cue is decided from what a key *did*, so a key that did nothing (a direction
//! with no menu open, an Enter while the gauge is not full) is silent, the way the game is.

use crate::battle::BattleCmd;
use crate::ui::menu::{MenuAction, NavKey};
use crate::window::{BattleKey, BattleRequest};

/// A menu sound effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Cue {
    /// The cursor moved.
    Cursor,
    /// A choice was made, or a level opened.
    Confirm,
    /// Back out of a level, or close.
    Cancel,
    /// A choice that cannot be made (not enough MP, nothing to use).
    Buzzer,
}

impl Cue {
    pub const ALL: [Cue; 4] = [Cue::Cursor, Cue::Confirm, Cue::Cancel, Cue::Buzzer];

    /// How loud it plays, `0.0..=1.0` (the effects share the master volume with the music: the
    /// click is the quietest, the buzzer is made to be noticed).
    pub fn gain(self) -> f32 {
        match self {
            Cue::Cursor => 0.55,
            Cue::Confirm => 0.75,
            Cue::Cancel => 0.7,
            Cue::Buzzer => 0.8,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Cue::Cursor => "cursor",
            Cue::Confirm => "confirm",
            Cue::Cancel => "cancel",
            Cue::Buzzer => "buzzer",
        }
    }
}

/// The refusals of [`crate::window::battle_request`] that are not a refusal at all: Enter while the
/// gauge is not full and Enter with no battle. The game does nothing then, and so is silent here.
pub const IDLE_REFUSALS: [&str; 2] = [crate::window::GAUGE_NOT_FULL, crate::window::NO_BATTLE_HINT];

/// The cue a battle key made, from what it came to.
pub fn battle_cue(key: BattleKey, request: &BattleRequest) -> Option<Cue> {
    match request {
        // The debug request for a battle (the B key) is not a menu choice.
        BattleRequest::Send(BattleCmd::RequestEncounter { .. }) => None,
        BattleRequest::Send(BattleCmd::Command { .. }) => Some(Cue::Confirm),
        BattleRequest::Menu => match key {
            BattleKey::Up | BattleKey::Down | BattleKey::Left | BattleKey::Right => Some(Cue::Cursor),
            BattleKey::Confirm => Some(Cue::Confirm),
            BattleKey::Back => Some(Cue::Cancel),
            BattleKey::Request => None,
        },
        BattleRequest::Nothing(why) => {
            let refused = !why.is_empty() && !IDLE_REFUSALS.contains(why) && matches!(key, BattleKey::Confirm | BattleKey::Back);
            refused.then_some(Cue::Buzzer)
        }
    }
}

/// The cue a key made in the character menu. `changed` says whether the menu's screen (the cursor,
/// the focused field, the question) is different afterwards, `on_list` whether the key was pressed on the
/// list of saved characters (where Delete opens a question; on the create form it edits the name),
/// `error` whether an error notice was put up by this key, and `action` is what the menu asked its host
/// to do. Typing and editing keys are silent.
pub fn menu_cue(key: NavKey, changed: bool, on_list: bool, action: Option<&MenuAction>, error: bool) -> Option<Cue> {
    match (key, action) {
        (_, Some(MenuAction::Quit)) => None,
        (_, Some(MenuAction::CancelConnect)) => Some(Cue::Cancel),
        (_, Some(MenuAction::Play { .. } | MenuAction::Register { .. } | MenuAction::Forget { .. })) => Some(Cue::Confirm),
        (NavKey::Up | NavKey::Down | NavKey::Left | NavKey::Right | NavKey::Tab | NavKey::Home | NavKey::End | NavKey::PageUp | NavKey::PageDown, None) => {
            changed.then_some(Cue::Cursor)
        }
        (NavKey::Enter, None) if error => Some(Cue::Buzzer),
        (NavKey::Enter, None) => changed.then_some(Cue::Confirm),
        (NavKey::Delete, None) => (on_list && changed).then_some(Cue::Confirm),
        (NavKey::Escape, None) => changed.then_some(Cue::Cancel),
        (NavKey::Backspace, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmv_protocol::battle::Command;
    use fmv_protocol::{ActorId, BattleId};

    fn command() -> BattleRequest {
        BattleRequest::Send(BattleCmd::Command { battle_id: BattleId(1), actor: ActorId(0), command: Command::Attack { target: ActorId(4) } })
    }

    #[test]
    fn a_battle_key_is_heard_for_what_it_did() {
        // Moving the cursor clicks, whichever way.
        for key in [BattleKey::Up, BattleKey::Down, BattleKey::Left, BattleKey::Right] {
            assert_eq!(battle_cue(key, &BattleRequest::Menu), Some(Cue::Cursor), "{key:?}");
        }
        // Opening a level or picking an entry confirms; backing out cancels.
        assert_eq!(battle_cue(BattleKey::Confirm, &BattleRequest::Menu), Some(Cue::Confirm));
        assert_eq!(battle_cue(BattleKey::Back, &BattleRequest::Menu), Some(Cue::Cancel));
        // The last step of a choice sends the command: that confirms too.
        assert_eq!(battle_cue(BattleKey::Confirm, &command()), Some(Cue::Confirm));
        // The debug request for a battle is not a menu choice.
        let ask = BattleRequest::Send(BattleCmd::RequestEncounter { formation: None });
        assert_eq!(battle_cue(BattleKey::Request, &ask), None);
        assert_eq!(battle_cue(BattleKey::Request, &BattleRequest::Menu), None);
    }

    #[test]
    fn a_choice_that_cannot_be_made_buzzes_and_an_idle_press_is_silent() {
        // A refusal with a reason (not enough MP, say) buzzes.
        assert_eq!(battle_cue(BattleKey::Confirm, &BattleRequest::Nothing("not enough MP")), Some(Cue::Buzzer));
        // The two idle answers do nothing, as in the game.
        for idle in IDLE_REFUSALS {
            assert_eq!(battle_cue(BattleKey::Confirm, &BattleRequest::Nothing(idle)), None, "{idle}");
        }
        // A direction nobody listens to, and B with a battle already on, are silent.
        assert_eq!(battle_cue(BattleKey::Up, &BattleRequest::Nothing("")), None);
        assert_eq!(battle_cue(BattleKey::Request, &BattleRequest::Nothing("you are already in a battle")), None);
        assert_eq!(battle_cue(BattleKey::Down, &BattleRequest::Nothing("some reason")), None, "only a Confirm can be refused");
    }

    #[test]
    fn the_character_menu_clicks_confirms_cancels_and_buzzes() {
        let play = MenuAction::Play { index: 0 };
        assert_eq!(menu_cue(NavKey::Down, true, true, None, false), Some(Cue::Cursor));
        assert_eq!(menu_cue(NavKey::Down, false, true, None, false), None, "a list of one does not move");
        assert_eq!(menu_cue(NavKey::Tab, true, false, None, false), Some(Cue::Cursor));
        assert_eq!(menu_cue(NavKey::Enter, false, true, Some(&play), false), Some(Cue::Confirm));
        assert_eq!(menu_cue(NavKey::Enter, true, true, None, false), Some(Cue::Confirm), "Enter that opened something");
        assert_eq!(menu_cue(NavKey::Enter, false, false, None, true), Some(Cue::Buzzer), "Enter that put up an error");
        assert_eq!(menu_cue(NavKey::Enter, false, false, None, false), None);
        assert_eq!(menu_cue(NavKey::Escape, true, false, None, false), Some(Cue::Cancel));
        assert_eq!(menu_cue(NavKey::Escape, false, false, Some(&MenuAction::CancelConnect), false), Some(Cue::Cancel));
        assert_eq!(menu_cue(NavKey::Escape, false, true, Some(&MenuAction::Quit), false), None, "leaving the client is silent");
        assert_eq!(menu_cue(NavKey::Delete, true, true, None, false), Some(Cue::Confirm), "the forget question opens");
        assert_eq!(menu_cue(NavKey::Delete, false, true, Some(&MenuAction::Forget { index: 0 }), false), Some(Cue::Confirm));
        // Editing the name is silent: Delete on the create form deletes a letter, it is not a choice.
        assert_eq!(menu_cue(NavKey::Delete, true, false, None, false), None);
        assert_eq!(menu_cue(NavKey::Backspace, true, false, None, false), None);
    }

    #[test]
    fn every_cue_has_a_name_and_a_sensible_level() {
        for cue in Cue::ALL {
            assert!(!cue.name().is_empty());
            assert!((0.1..=1.0).contains(&cue.gain()), "{cue:?}");
        }
        assert_eq!(Cue::ALL.len(), 4);
        assert!(Cue::Cursor.gain() < Cue::Buzzer.gain());
    }
}
