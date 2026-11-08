//! Each account's own character progress: which character it plays and the level, experience, AP
//! and gil that character has earned (milestone M2-d). Pure rules here, no I/O: the world keeps
//! one [`Progress`] per player, and `saver` writes the results to the store.
//!
//! # Whose numbers these are
//!
//! * **The character** an account plays follows its avatar's look ([`character_for_look`]: for FF7
//!   the `ff7/<name>` appearance id, default Cloud). It replaces the interim rule that gave the
//!   `i`-th member of a battle the `i`-th character of the game, which made a player a different
//!   character in every battle.
//! * **The level** is the account's own. A battle's `EncounterStart` names each member's character
//!   and level, so every peer builds the same combatants from its own game files.
//! * **A look changes who the account plays, not what it has earned.** The level, experience, AP and
//!   gil carry over to the other character; its curve decides the next level from there (a character
//!   whose curve asks for less than the account already has levels up at its next win). Nothing is
//!   ever taken away.
//! * **Not the story.** This is separate from the account's `Progression` blob, which is also its
//!   story phase key (see `accounts::phase_key`): nothing here can move a player to another instance.
//!
//! # Rewards
//!
//! A victory pays the members that fought on ([`fmv_protocol::split_rewards`]: experience and gil
//! split among the living, the AP in full). [`apply_reward`] adds a payment, saturating at the
//! game's caps ([`fmv_protocol::ProgressLimits`]; the store checks none of them), then raises the
//! level for every threshold of the character's experience curve ([`BattleRules::exp_for_level`])
//! the experience has reached. A defeat pays nothing and takes nothing.
//!
//! The server holds the mirror and applies each payment to it, and the saver applies the same
//! payment to the stored record as an atomic read-modify-write
//! (`Store::update_character_progress`). They start equal and receive the same payments in the same
//! order, so they agree, and a payment for an account that is no longer connected (it left
//! mid-battle; its character fought on) still reaches the store. **The store is the authority**, and
//! the mirror can lag it in one narrow case: a player that wins, disconnects and logs in again before
//! the saver's write has landed reads the old record, so its mirror (and the level of its next
//! `EncounterStart`) is missing that payment until its next login. Payments are deltas, so the store
//! itself never loses one.
//!
//! # Defaults
//!
//! An account with no stored record starts at its character's starting level and experience from the
//! game's own data ([`BattleRules::starting_level`], [`BattleRules::starting_exp`]) with no AP and no
//! gil. Nothing is written until the first payment, so there is nothing to migrate.

use fmv_core::{GameId, InventoryEntry};
use fmv_persist::CharacterProgress;
use fmv_protocol::{BattleReward, BattleRules, ProgressLimits, ServerMsg};

/// The character of an account whose look names none (FF7: Cloud).
pub const DEFAULT_CHARACTER: u8 = 0;

/// FF7's character ids (`fmv_ff7_battle::kernel::CharacterId::index`) of the looks the game offers
/// (`fmv_ff7::playable`). Cait Sith has no field model, so no look and no row here. A test checks
/// the table against the playable models, so a look added without a row fails there.
const FF7_LOOKS: [(&str, u8); 8] = [
    ("ff7/cloud", 0),
    ("ff7/barret", 1),
    ("ff7/tifa", 2),
    ("ff7/aerith", 3),
    ("ff7/red_xiii", 4),
    ("ff7/yuffie", 5),
    ("ff7/vincent", 7),
    ("ff7/cid", 8),
];

/// The character an avatar with this look plays in `game`. `look` is what the server shows for the
/// avatar (`AvatarAppearance::appearance_ref`); a look the game does not know, none, or a game with
/// no characters of its own gives [`DEFAULT_CHARACTER`].
pub fn character_for_look(game: GameId, look: Option<&str>) -> u8 {
    match (game, look) {
        (GameId::Ff7, Some(look)) => {
            let look = look.trim();
            FF7_LOOKS
                .iter()
                .find(|(id, _)| id.eq_ignore_ascii_case(look))
                .map_or(DEFAULT_CHARACTER, |&(_, character)| character)
        }
        _ => DEFAULT_CHARACTER,
    }
}

/// What a player has earned with its character: the world's mirror of the stored record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    /// The game's id of the character (FF7: `CharacterId::index`).
    pub character: u8,
    pub level: u8,
    /// Total experience.
    pub exp: u32,
    pub ap: u32,
    pub gil: u32,
}

impl Progress {
    pub fn from_record(record: &CharacterProgress) -> Self {
        Self { character: record.character, level: record.level, exp: record.exp, ap: record.ap, gil: record.gil }
    }

    /// The record to store for `account` in `game`: the numbers, and NO inventory (`None`). A record
    /// replaces the whole file, so whoever stores one over an existing record must carry the stored
    /// inventory over (see `saver::persist`); an inventory that is silently dropped would be lost.
    pub fn to_record(self, account: fmv_core::AccountId, game: GameId, now: u64) -> CharacterProgress {
        CharacterProgress {
            exp: self.exp,
            ap: self.ap,
            gil: self.gil,
            ..CharacterProgress::new(account, game, self.character, self.level, now)
        }
    }
}

/// What the login found out about an account's progress: who it plays, and what the store held.
/// The world turns it into a [`Progress`] when the player joins ([`resolve`]) and into an
/// [`crate::inventory::Inventory`] with the game's starting stock where the store had none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgressSeed {
    /// The character its look names ([`character_for_look`]).
    pub character: u8,
    /// The stored record, `None` if the account has never earned anything.
    pub stored: Option<Progress>,
    /// The inventory the stored record holds. `None` is "never stored" (no record, or one of an older build
    /// that had no inventory): the player then starts with the game's starting stock (see
    /// `inventory`), which is not the same as `Some` of an empty list.
    pub inventory: Option<Vec<InventoryEntry>>,
}

impl ProgressSeed {
    /// A seed with no stored inventory.
    pub fn of(character: u8, stored: Option<Progress>) -> Self {
        Self { character, stored, inventory: None }
    }
}

impl Default for ProgressSeed {
    fn default() -> Self {
        Self::of(DEFAULT_CHARACTER, None)
    }
}

/// A new account's progress as a `character`: its starting level and experience, nothing else.
pub fn fresh(rules: &dyn BattleRules, character: u8) -> Progress {
    let limits = rules.progress_limits();
    let level = rules.starting_level(character).clamp(1, limits.max_level);
    let exp = rules.starting_exp(character, level).min(limits.max_exp);
    Progress { character, level, exp, ap: 0, gil: 0 }
}

/// Bring a progress inside the game's caps. What is stored is not trusted to fit them: a hand-edited
/// file, or a record written under other rules, must never put a level on the wire that no combatant
/// can be built for.
pub fn clamp(limits: ProgressLimits, progress: &mut Progress) {
    progress.level = progress.level.clamp(1, limits.max_level);
    progress.exp = progress.exp.min(limits.max_exp);
    progress.gil = progress.gil.min(limits.max_gil);
    progress.ap = progress.ap.min(limits.max_ap);
}

/// The mirror of a player that joins: its stored progress (or a fresh start) as the character its look
/// names, inside the game's caps.
pub fn resolve(rules: &dyn BattleRules, seed: &ProgressSeed) -> Progress {
    match seed.stored {
        Some(mut stored) => {
            stored.character = seed.character;
            clamp(rules.progress_limits(), &mut stored);
            stored
        }
        None => fresh(rules, seed.character),
    }
}

/// Add one payment to `progress`: experience, gil and AP up to the caps, then every level the
/// experience has reached. `true` if the level rose. A character without an experience curve in
/// these rules keeps its level.
pub fn apply_reward(rules: &dyn BattleRules, progress: &mut Progress, reward: &BattleReward) -> bool {
    let limits = rules.progress_limits();
    progress.exp = progress.exp.saturating_add(reward.exp).min(limits.max_exp);
    progress.gil = progress.gil.saturating_add(reward.gil).min(limits.max_gil);
    progress.ap = progress.ap.saturating_add(reward.ap).min(limits.max_ap);
    let before = progress.level;
    while progress.level < limits.max_level {
        match rules.exp_for_level(progress.character, progress.level + 1) {
            Some(threshold) if progress.exp >= threshold => progress.level += 1,
            _ => break,
        }
    }
    progress.level > before
}

/// Experience still missing for the next level; 0 at the highest level, and for a character whose
/// rules have no curve.
pub fn exp_to_next(rules: &dyn BattleRules, progress: &Progress) -> u32 {
    if progress.level >= rules.progress_limits().max_level {
        return 0;
    }
    rules
        .exp_for_level(progress.character, progress.level.saturating_add(1))
        .map_or(0, |threshold| threshold.saturating_sub(progress.exp))
}

/// The `CharacterProgress` message that tells a player where it stands.
pub fn message(rules: &dyn BattleRules, progress: &Progress, leveled_up: bool) -> ServerMsg {
    ServerMsg::CharacterProgress {
        character: progress.character,
        level: progress.level,
        exp: progress.exp,
        exp_to_next: exp_to_next(rules, progress),
        gil: progress.gil,
        ap: progress.ap,
        leveled_up,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmv_core::AccountId;
    use fmv_protocol::SyntheticRules;

    /// `SyntheticRules`: every character starts at level 5 with 40 experience; level 6 needs 60,
    /// level 7 needs 84, level 8 needs 112 (`2 * level * (level - 1)`).
    fn rules() -> SyntheticRules {
        SyntheticRules::default()
    }

    fn reward(exp: u32, gil: u32, ap: u32) -> BattleReward {
        BattleReward::new(AccountId(1), exp, gil, ap)
    }

    #[test]
    fn every_look_the_game_offers_names_its_own_distinct_character() {
        let models = fmv_ff7::playable::PLAYABLE_FIELD_MODELS;
        assert_eq!(models.len(), FF7_LOOKS.len(), "a look without a character (or the reverse)");
        let mut seen = std::collections::BTreeSet::new();
        for model in models {
            let character = character_for_look(GameId::Ff7, Some(model.appearance_ref));
            assert!(seen.insert(character), "{} shares its character", model.appearance_ref);
            // The id is a real FF7 character and its name is the look's (Cloud Strife, Aerith
            // Gainsborough, Red XIII, ...): the table cannot mix two characters up.
            let who = fmv_ff7_battle::kernel::CharacterId::from_index(usize::from(character)).expect("a character id");
            assert!(model.display_name.starts_with(who.name()), "{} is not {}", model.display_name, who.name());
        }
        assert_eq!(character_for_look(GameId::Ff7, Some("ff7/cloud")), 0);
        assert_eq!(character_for_look(GameId::Ff7, Some("ff7/tifa")), 2);
        assert_eq!(character_for_look(GameId::Ff7, Some("FF7/Vincent ")), 7, "the same spelling rule as the look itself");
    }

    #[test]
    fn no_look_an_unknown_look_or_another_game_plays_the_default_character() {
        for (game, look) in [
            (GameId::Ff7, None),
            (GameId::Ff7, Some("")),
            (GameId::Ff7, Some("../../etc/passwd")),
            (GameId::Ff7, Some("ff8/squall")),
            (GameId::Ff7, Some("ff7/cait_sith")),
            (GameId::Fixture, Some("ff7/tifa")),
            (GameId::Ff8, Some("ff7/barret")),
        ] {
            assert_eq!(character_for_look(game, look), DEFAULT_CHARACTER, "{game:?} {look:?}");
        }
    }

    #[test]
    fn a_new_account_starts_where_the_game_says_with_nothing_earned() {
        let p = fresh(&rules(), 2);
        assert_eq!(p, Progress { character: 2, level: 5, exp: 40, ap: 0, gil: 0 });
        assert_eq!(resolve(&rules(), &ProgressSeed::of(2, None)), p);
        assert_eq!(resolve(&rules(), &ProgressSeed::default()).character, DEFAULT_CHARACTER);
    }

    #[test]
    fn a_stored_record_wins_and_follows_the_look_but_keeps_what_was_earned() {
        let stored = Progress { character: 0, level: 9, exp: 700, ap: 31, gil: 2500 };
        let p = resolve(&rules(), &ProgressSeed::of(3, Some(stored)));
        assert_eq!(p, Progress { character: 3, ..stored }, "who it plays changed, what it earned did not");
    }

    #[test]
    fn what_is_stored_is_brought_inside_the_games_caps() {
        let limits = rules().progress_limits();
        let hostile = Progress { character: 0, level: 255, exp: u32::MAX, ap: u32::MAX, gil: u32::MAX };
        let p = resolve(&rules(), &ProgressSeed::of(0, Some(hostile)));
        assert_eq!(p, Progress { character: 0, level: limits.max_level, exp: limits.max_exp, ap: limits.max_ap, gil: limits.max_gil });
        let zero = Progress { character: 0, level: 0, exp: 0, ap: 0, gil: 0 };
        assert_eq!(resolve(&rules(), &ProgressSeed::of(0, Some(zero))).level, 1, "level 0 never exists");
    }

    #[test]
    fn a_payment_adds_up_and_a_level_is_gained_exactly_at_its_threshold() {
        let rules = rules();
        let mut p = fresh(&rules, 0); // level 5, 40 exp; level 6 at 60
        assert_eq!(exp_to_next(&rules, &p), 20);
        assert!(!apply_reward(&rules, &mut p, &reward(19, 10, 1)));
        assert_eq!(p, Progress { character: 0, level: 5, exp: 59, ap: 1, gil: 10 });
        assert_eq!(exp_to_next(&rules, &p), 1);
        // One more point is level 6, and what is left over counts towards level 7 (84).
        assert!(apply_reward(&rules, &mut p, &reward(1, 0, 0)));
        assert_eq!((p.level, p.exp), (6, 60));
        assert_eq!(exp_to_next(&rules, &p), 24);
        assert!(!apply_reward(&rules, &mut p, &reward(0, 5, 5)), "nothing earned in experience, nothing gained");
        assert_eq!((p.level, p.exp, p.gil, p.ap), (6, 60, 15, 6));
    }

    #[test]
    fn a_big_payment_gains_every_level_it_reaches() {
        let rules = rules();
        let mut p = fresh(&rules, 0);
        // 40 + 80 = 120: past 60 (6), 84 (7) and 112 (8), short of 144 (9).
        assert!(apply_reward(&rules, &mut p, &reward(80, 0, 0)));
        assert_eq!((p.level, p.exp), (8, 120));
        assert_eq!(exp_to_next(&rules, &p), 24);
    }

    #[test]
    fn nothing_goes_past_the_caps_and_the_top_level_has_nothing_next() {
        let rules = rules();
        let limits = rules.progress_limits();
        let mut p = Progress { character: 0, level: limits.max_level - 1, exp: limits.max_exp - 5, ap: limits.max_ap - 1, gil: limits.max_gil - 1 };
        assert!(apply_reward(&rules, &mut p, &reward(u32::MAX, u32::MAX, u32::MAX)));
        assert_eq!(p, Progress { character: 0, level: limits.max_level, exp: limits.max_exp, ap: limits.max_ap, gil: limits.max_gil });
        assert_eq!(exp_to_next(&rules, &p), 0);
        // At the top, more experience changes only the (capped) total.
        assert!(!apply_reward(&rules, &mut p, &reward(1, 1, 1)));
        assert_eq!(p.level, limits.max_level);
    }

    #[test]
    fn a_character_the_rules_have_no_curve_for_gains_experience_but_never_a_level() {
        let rules = rules();
        // Character 9 does not exist in the synthetic rules: no curve.
        let mut p = Progress { character: 9, level: 4, exp: 0, ap: 0, gil: 0 };
        assert!(!apply_reward(&rules, &mut p, &reward(1_000_000, 7, 3)));
        assert_eq!((p.level, p.exp, p.gil, p.ap), (4, 1_000_000, 7, 3));
        assert_eq!(exp_to_next(&rules, &p), 0);
    }

    /// A look change moves the account to the other character's curve; what it has stays.
    #[test]
    fn a_curve_that_asks_for_less_than_the_account_has_levels_up_at_the_next_win() {
        let rules = rules();
        // 70 experience at level 5 is what a level-6 character (60) has, but this one is level 5.
        let mut p = Progress { character: 0, level: 5, exp: 70, ap: 0, gil: 0 };
        assert_eq!(exp_to_next(&rules, &p), 0, "already past the threshold");
        assert!(apply_reward(&rules, &mut p, &reward(0, 0, 0)), "a payment of nothing still settles the level");
        assert_eq!(p.level, 6);
    }

    #[test]
    fn the_message_carries_exactly_what_the_player_has() {
        let rules = rules();
        let p = Progress { character: 3, level: 6, exp: 64, ap: 4, gil: 40 };
        assert_eq!(
            message(&rules, &p, true),
            ServerMsg::CharacterProgress { character: 3, level: 6, exp: 64, exp_to_next: 20, gil: 40, ap: 4, leveled_up: true }
        );
        assert!(matches!(message(&rules, &p, false), ServerMsg::CharacterProgress { leveled_up: false, .. }));
    }

    #[test]
    fn records_round_trip_through_the_mirror() {
        let p = Progress { character: 2, level: 12, exp: 1234, ap: 9, gil: 77 };
        let record = p.to_record(AccountId(4), GameId::Ff7, 55);
        assert_eq!((record.account_id, record.game, record.updated_at), (AccountId(4), GameId::Ff7, 55));
        assert_eq!(Progress::from_record(&record), p);
    }
}
