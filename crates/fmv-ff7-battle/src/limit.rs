//! FF7 Limit Breaks: the limit gauge a character fills by taking damage, and the first limit of each
//! character (Braver, Big Shot, Beat Rush, Healing Wind, ...). The battle engine ([`crate::rules`])
//! plays them; this module is the data and the arithmetic.
//!
//! # What is in the owner's `kernel.bin`, and what is not
//!
//! **In it (read, never assumed):** for every character, the ids of its limits by limit level and slot,
//! the kills and uses that earn them and the **HP divisor** of each limit level, which says how slowly
//! the gauge fills ([`crate::kernel::CharacterLimits`], the 56-byte growth record, bytes `0x0C..0x38`).
//! The id of a limit is its index in the "Names - magic" text table (Braver is 128, Big Shot 135, Beat
//! Rush 149, Healing Wind 142, ...), so a name is always the machine's own text. Checked on the owner's
//! Steam copy by `tests/real_data.rs`: the nine first limits, the names, and that the kills and uses
//! reproduce GideonD's published requirements (Cloud: 8 uses, 120 kills, 7 uses, 320 kills, 6 uses).
//!
//! **Not in it:** the damage of a limit break. The kernel's 128 attack records end before the limit ids
//! (128 and up), `scene.bin` has no such id and `battle.lgp` holds only models, so Braver's Power is
//! not in any file the owner imported (FF7 keeps it elsewhere). Tifa's, Cait Sith's and Vincent's
//! moves have records of their own at indexes 98 to 121 of the kernel's attack data, which is how this
//! module could check the community's numbers against real data (see "Damage" below).
//!
//! # The limit gauge
//!
//! A character's gauge runs `0..=`[`LIMIT_MAX`] (255: the game treats it as a whole number, empty to
//! full). It rises when an enemy hurts the character and falls to 0 when the character uses its limit
//! or is knocked out; healing never lowers it, and it carries over between battles (the server keeps it
//! per account). With the gauge full and its ATB gauge full, the character may give the Limit command
//! instead of Attack.
//!
//! The amount a hit adds is [`limit_gain`]:
//!
//! ```text
//! gain = min(255, damage * 76800 / (max HP * HP divisor of the limit level))
//! ```
//!
//! **APPROXIMATION: the constant 76800 is calibrated, not read from a disassembly.** The community
//! documentation says four things affect the gain: the damage, the character's max HP, its limit level
//! (through the kernel's per-level HP divisor) and Fury/Sadness; and gives the cost of a full gauge as a
//! fraction of max HP (Final Fantasy Wiki, "Limit (Final Fantasy VII)", from Terence Fergusson's *FF7
//! Party Mechanics* guide, whose text could not be fetched here): at limit level 2 Red XIII and Yuffie
//! (divisor 300 in the owner's file) must lose "just beneath" 99.6% of their max HP and Cait Sith
//! (divisor 180) 59.8%; every character must lose more than their max HP at levels 3 and 4, and Cloud
//! and Tifa at level 2 (divisors 324 and 334). A gain linear in `damage / (max HP * divisor)` with the
//! constant 76800 gives 255/256 = 99.61% at divisor 300, 59.77% at 180 and over 100% for every
//! divisor the file lists at those levels, which is all of that (the constant is `256 * 300`), and the
//! rest of the gauge's behaviour follows (level 1: Cloud's 140 means about 46% of his max HP, Tifa's
//! 120 about 40%, Aerith's 200 about 66%). Fury and Sadness (which double and halve the gain) are
//! statuses and not modelled. Integer arithmetic; a hit smaller than a point of gauge adds nothing.
//!
//! Only damage dealt **by an enemy to a party member** counts (the game ignores characters hitting
//! each other). **The limit level is fixed at 1**: every character fights with its first limit and the
//! level-1 divisor, because the levels above are earned by kills and uses that the account does not
//! track (see "Not modelled").
//!
//! # Damage of the first limits (APPROXIMATION from community documentation)
//!
//! Every limit break here is one action that never misses, cannot be a critical hit and ignores the
//! rows, on one enemy (Healing Wind: on the whole party); the damage ones use the standard physical
//! formula (`Power * (512 - Def) * Base / (16 * 512)`, with the random variance) at a Power of `16 * the
//! multiplier of a normal attack` that the community gives (GideonD's *Limit Break Mechanics* guide, and
//! the Final Fantasy Wiki and Jegged's per-limit pages):
//!
//! | character | limit (id) | here |
//! |---|---|---|
//! | Cloud | Braver (128) | one hit, 3.00x (Power 48) |
//! | Barret | Big Shot (135) | one hit, 3.25x (Power 52) |
//! | Tifa | Beat Rush (149) | three Hits of 1.25x (the kernel's record 98: Power 20) as one hit, 3.75x (Power 60); the slot reels (Miss, Hit, Yeah!) are not modelled |
//! | Aerith | Healing Wind (142) | heals **every** ally, each for half of its own max HP, at full strength: the community's "Formula: Max HP%, Pwr: [Target's Max HP / 2], Tar: All Al (NS)" (Terence Fergusson's *FF7 Party Mechanics Guide*, Aerith's limit breaks). It hits the whole party only ([`Aim::Side`]: `Target::AllAllies`), and a fraction of max HP is never split |
//! | Red XIII | Sled Fang (163) | one hit, 3.00x (Power 48) |
//! | Yuffie | Greased Lightning (177) | one hit, 3.00x (Power 48) |
//! | Cid | Boost Jump (156) | one hit, 3.25x (Power 52) |
//! | Vincent | Galian Beast (173) | a transformation in FF7; here the move that GideonD says its first attack is, Beast Flare (the kernel's record 113: Power 60, fire, magical, 3.75x), on one enemy, with the Spell formula |
//! | Cait Sith | Dice (170) | none: its damage is a roll of several dice that is not modelled (and she has no look, so no account plays her) |
//!
//! `tests/real_data.rs` pins the two kernel records the table leans on (98 and 113).
//!
//! # Not modelled
//!
//! Limit levels 2 to 4 and the kills and uses that earn them, Fury and Sadness, the second limit of
//! a level, Tifa's slot reels, Vincent's transformations, Cait Sith's dice, the whole-side limits of the
//! higher levels (Breath of the Earth, Grenade Bomb, ...) and limit-break animations.

use crate::action::{element, ActionSpec, Aim};
use crate::kernel::{CharacterId, Kernel};

/// The full limit gauge.
pub const LIMIT_MAX: u16 = 255;

/// The calibrated constant of [`limit_gain`] (`256 * 300`, see the module docs).
pub const GAIN_SCALE: u64 = 76_800;

/// What a hit of `damage` on a character with `max_hp` adds to its limit gauge, when its limit level's
/// HP divisor is `divisor`: `damage * 76800 / (max_hp * divisor)`, at most [`LIMIT_MAX`]. Zero when the
/// character has no HP or no divisor (nothing to fill).
pub fn limit_gain(damage: u32, max_hp: u32, divisor: u32) -> u16 {
    let denominator = u64::from(max_hp) * u64::from(divisor);
    if denominator == 0 {
        return 0;
    }
    let gain = u64::from(damage) * GAIN_SCALE / denominator;
    u16::try_from(gain.min(u64::from(LIMIT_MAX))).unwrap_or(LIMIT_MAX)
}

/// What a battle menu needs to know about a character's limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LimitInfo {
    /// The limit's id: its index in the machine's own "Names - magic" text table (also what
    /// [`crate::rules::ActionRef::Limit`] carries).
    pub attack: u16,
    /// Aimed at the party (a healing limit: Healing Wind) instead of the enemies.
    pub targets_allies: bool,
    /// How many it hits: one target, or the whole party only (Healing Wind is [`Aim::Side`]: it is given
    /// with `Target::AllAllies`).
    pub aim: Aim,
}

/// A character's limit break as the battle plays it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LimitBreak {
    /// The limit's id (see [`LimitInfo::attack`]).
    pub attack: u16,
    /// What it does to its one target.
    pub spec: ActionSpec,
    /// The HP divisor of the character's limit level (see [`limit_gain`]); never zero.
    pub divisor: u32,
}

impl LimitBreak {
    /// A hand-made limit (tests and rule sets with no game files). A `divisor` of 0 is raised to 1.
    pub fn new(attack: u16, spec: ActionSpec, divisor: u32) -> Self {
        Self { attack, spec, divisor: divisor.max(1) }
    }

    /// What a menu needs to know.
    pub fn info(&self) -> LimitInfo {
        LimitInfo { attack: self.attack, targets_allies: self.spec.heals(), aim: self.spec.aim }
    }

    /// The first limit of `who` (limit level 1, slot 1) from the kernel's growth record: its id and the
    /// level-1 HP divisor are the file's, what it does is [`first_limit_spec`]. `None` for a character
    /// without a first limit in the file or one whose limit is not modelled (Cait Sith).
    pub fn for_character(kernel: &Kernel, who: CharacterId) -> Option<Self> {
        let limits = kernel.limits(who)?;
        let attack = u16::from(limits.first()?);
        let divisor = limits.divisor_at_level_one()?;
        Some(Self { attack, spec: first_limit_spec(who)?, divisor })
    }
}

/// What the first limit of `who` does (see the module docs for the sources). `None`: not modelled.
pub fn first_limit_spec(who: CharacterId) -> Option<ActionSpec> {
    Some(match who {
        CharacterId::Cloud | CharacterId::RedXiii | CharacterId::Yuffie => ActionSpec::limit_strike(48),
        CharacterId::Barret | CharacterId::Cid => ActionSpec::limit_strike(52),
        CharacterId::Tifa => ActionSpec::limit_strike(60),
        CharacterId::Aeris => ActionSpec::limit_heal(16),
        CharacterId::Vincent => ActionSpec::limit_blast(60, element::FIRE),
        CharacterId::CaitSith => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The documented anchors: at divisor 300 a full gauge costs just under 99.6% of max HP, at 180 just
    /// under 59.8% (Red XIII and Yuffie, Cait Sith at limit level 2).
    #[test]
    fn the_gauge_fills_at_the_documented_fractions_of_max_hp() {
        let fills_at = |max_hp: u32, divisor: u32| (1..=max_hp * 3).find(|d| limit_gain(*d, max_hp, divisor) == LIMIT_MAX).expect("it fills");
        // 1000 max HP: 255/256 of it is 996.09, so 997 points of damage fill it.
        assert_eq!(fills_at(1000, 300), 997);
        assert_eq!(limit_gain(996, 1000, 300), 254);
        // 59.77%: 598 points.
        assert_eq!(fills_at(1000, 180), 598);
        // More than max HP at the divisors of Cloud's and Tifa's level 2 and of every level 3 and 4 divisor.
        for divisor in [324, 334, 374, 322, 319, 435, 506] {
            assert!(fills_at(1000, divisor) > 1000, "divisor {divisor}");
        }
        // Level 1 of Cloud (140), Tifa (120) and Aerith (200): about 46%, 40% and 66% of max HP.
        assert_eq!([140, 120, 200].map(|d| fills_at(1000, d)), [465, 399, 665]);
    }

    #[test]
    fn the_gain_is_linear_capped_and_total() {
        assert_eq!(limit_gain(0, 500, 140), 0);
        // 76800 / (500 * 140) = 1.097 points per point of damage.
        assert_eq!(limit_gain(10, 500, 140), 10);
        assert_eq!(limit_gain(100, 500, 140), 109);
        assert_eq!(limit_gain(u32::MAX, 1, 1), LIMIT_MAX, "never above a full gauge");
        // A hit too small for a whole point adds nothing.
        assert_eq!(limit_gain(1, 9999, 506), 0);
        // Nothing to fill: no HP, no divisor.
        assert_eq!(limit_gain(500, 0, 140), 0);
        assert_eq!(limit_gain(500, 500, 0), 0);
        // A hit twice as hard adds (about) twice as much, and a bigger character fills more slowly.
        assert!(limit_gain(50, 400, 140) >= 2 * limit_gain(25, 400, 140) - 1);
        assert!(limit_gain(50, 800, 140) < limit_gain(50, 400, 140));
        assert!(limit_gain(50, 400, 506) < limit_gain(50, 400, 140), "a higher limit level fills more slowly");
    }

    #[test]
    fn every_character_but_cait_sith_has_a_first_limit_that_acts_on_one_target() {
        for who in CharacterId::ALL {
            match first_limit_spec(who) {
                None => assert_eq!(who, CharacterId::CaitSith),
                Some(spec) => {
                    assert_eq!(spec.mp_cost, 0, "{who:?}: a limit costs no MP");
                    assert!(spec.aim.allows_one() != spec.aim.allows_side() && !spec.rolls_accuracy && !spec.can_crit, "{who:?}");
                    assert_eq!(spec.heals(), who == CharacterId::Aeris, "only Healing Wind heals");
                    // Healing Wind is the one that hits a whole side, the whole party, and only that.
                    assert_eq!(spec.aim, if who == CharacterId::Aeris { Aim::Side } else { Aim::One }, "{who:?}");
                }
            }
        }
        // The powers are 16 * the community's multipliers.
        let power = |who| first_limit_spec(who).map(|s| s.power);
        assert_eq!(power(CharacterId::Cloud), Some(48));
        assert_eq!(power(CharacterId::Barret), Some(52));
        assert_eq!(power(CharacterId::Aeris), Some(16), "half of max HP: 16/32");
        let beast = first_limit_spec(CharacterId::Vincent).expect("Vincent");
        assert_eq!((beast.power, beast.element_mask), (60, element::FIRE));
    }

    #[test]
    fn a_hand_made_limit_never_has_a_zero_divisor() {
        let limit = LimitBreak::new(128, ActionSpec::limit_strike(48), 0);
        assert_eq!(limit.divisor, 1);
        assert_eq!(limit.info(), LimitInfo { attack: 128, targets_allies: false, aim: Aim::One });
        let heal = LimitBreak::new(142, ActionSpec::limit_heal(16), 200);
        assert!(heal.info().targets_allies);
        assert_eq!(heal.info().aim, Aim::Side, "Healing Wind heals the whole party");
    }
}
