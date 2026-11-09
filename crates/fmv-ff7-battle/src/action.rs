//! What an action is: the decoded damage-calculation byte of an attack or item record, the spells a
//! party member knows, the attacks an enemy picks from, elemental affinities and the item table.
//!
//! Everything here is data derived from the owner's own files (or hand-made in tests) and is plain,
//! `Copy`-friendly, total on any input: [`ActionSpec::from_attack`] and [`ActionSpec::from_item`] return
//! `None` for a record the rules do not model (a scripted special, a status the rules do not play) instead of
//! guessing, which is how "skip it safely" is decided in one place. The state machine that plays an
//! action is in [`crate::rules`].
//!
//! # The damage-calculation byte
//!
//! Sources: the table at <https://wiki.ffrtt.ru/index.php/FF7/Battle/Damage_Calculation> and the
//! disassembly pseudocode at <https://wiki.ffrtt.ru/index.php/FF7/DamageFormula>. The **high nybble**
//! says what kind of action it is and how it can miss, the **low nybble** the formula:
//!
//! | high | kind | accuracy |
//! |---|---|---|
//! | `0`, `3` | physical | never misses |
//! | `1`, `A` | physical | accuracy roll, can be a critical hit |
//! | `B` | physical | accuracy roll |
//! | `2` | magical | accuracy roll |
//! | `4`, `5` | magical | never misses |
//!
//! (`6`/`7` special damage, `8`/`9` level-based and Manipulate accuracy are not modelled.)
//!
//! | low | [`Formula`] | damage before defense |
//! |---|---|---|
//! | `1` | [`Formula::Standard`] | `(Power/16) * (Stat + ((Level+Stat)/32) * ((Level*Stat)/32))` |
//! | `2` | [`Formula::Spell`] | `(Power/16) * ((Level+Stat) * 6)` |
//! | `3` | [`Formula::CurrentHp`] | target's current HP `* Power / 32` (no defense) |
//! | `4` | [`Formula::MaxHp`] | target's max HP `* Power / 32` (no defense) |
//! | `5` | [`Formula::Restore`] | `22 * Power + 6 * (Level + Stat)` (no defense) |
//! | `6` | [`Formula::Flat`] | `20 * Power` (no defense) |
//! | `7` | [`Formula::Fixed`] | `(512 - Defense) * Power / 32` |
//! | `8` | [`Formula::Full`] | everything (Elixir) |
//!
//! "Stat" is Strength (plus weapon) for physical actions and Magic for magical ones, "Defense" the
//! matching defense. Formulas 1, 2, 5 and 7 are followed by the random variance `(3841 + 0..=255)/4096`;
//! 3, 4, 6 and 8 are not. A `Power` of 0, or a byte with no formula (`0x50`, the `0xFF` of a record with no
//! damage part), means "no damage, only an effect": a **status-only** action ([`Formula::None`], see "Statuses"
//! below), which is not modelled when the statuses it has are not ones the rules play.
//! A record whose element is Restorative ([`element::RESTORATIVE`]) with formula 4, 5, 6 or 8 **restores**
//! ([`ActionSpec::heals`]): Cure and Life and every potion are such records.
//!
//! # The special flags and targeting bits
//!
//! The 16-bit special flags are **active low** (<https://wiki.ffrtt.ru/index.php/FF7/Battle/Special_Attack_Flags>):
//! a cleared `0x0001` means the action hits MP instead of HP (Ether), a cleared `0x0100` that it only
//! hits the dead (Life, Phoenix Down), a cleared `0x0400` that it ignores defense. Targeting bits
//! (<https://wiki.ffrtt.ru/index.php/FF7/Battle/Targeting_Data>): `0x04` is "multiple targets by default"
//! and `0x08` "toggle multiple/single", which give an action's [`Aim`]: neither is one target, both is
//! one target or a whole side (Cure, Fire: "All Tar/1 Tar", which needs the All materia when it is a
//! spell), `0x04` alone is a whole side only ("All Tar (NS)": Ultima, Megalixir, Healing Wind) and `0x80`
//! is a random target (not offered). `0x20` is "short range" (halved against the back row).
//!
//! # Hitting a whole side
//!
//! An action that hits more than one target is **split**: its damage (or healing) is multiplied by 2/3
//! for every target ([`ActionSpec::splits`] says when). The rule is Terence Fergusson's *FF7 Battle
//! Mechanics* FAQ (v1.10, "Formula Modifiers", Split Damage), and the disassembly-derived pseudocode at
//! <https://wiki.ffrtt.ru/index.php/FF7/DamageFormula> ("SplitDamage") has the same structure: the
//! physical formula (`1`) and Cure's (`5`) always split when they hit several targets, the spell formula
//! (`2`) only when the spell can be toggled between one target and all of them (a forced whole side, "NS"
//! for "No Split", has full strength), and the formulas of a fraction of HP (`3`, `4`), flat amounts (`6`,
//! `7`) and "everything" (`8`) never split. A hit that has only one target is never split, even when it
//! was aimed at "all" and one enemy was left. APPROXIMATION to note: the pseudocode page writes the
//! split as `Damage * 3 / 2`, which is read as a transcription slip (the FAQ, the Final Fantasy Wiki and
//! the community all say 2/3), and the same FAQ says the chance of a status that comes with a split
//! attack is cut to 2/3 too, which [`crate::rules`] does (see [`crate::status::StatusEffect::chance_among`]).
//!
//! # Statuses
//!
//! A record's status mask and chance byte decode to a [`StatusEffect`] ([`crate::status`] has the layout and the
//! statuses the rules play); an action carries it in [`ActionSpec::status`]. A damaging or restoring action with
//! one tries it on every target it hit that still stands; a **status-only** action (Sleep, Haste, Poisona, an
//! Antidote or an Echo Screen) has nothing else, and rolls to hit like a spell (an accuracy roll when its
//! damage-calculation byte has one, never against an ally). Which side a status-only action aims at is the
//! record's own targeting bit `0x02` ("the cursor starts on the enemies"): clear for a buff or a cure
//! ([`ActionSpec::on_allies`]), set for a curse. A record whose only statuses are ones the rules do not play
//! decodes exactly as it did before they were modelled.
//!
//! # Elements
//!
//! An attack's 16-bit element mask ([`element`]) is matched against the target's affinities
//! ([`ElementAffinity`], from the enemy record: the element index and a rate code). When several of an
//! action's elements have an affinity the first of this order wins: Absorb, Full heal, Nullify, Death,
//! Half, Double. (The documents give no precedence; real spells have one element, so it only matters for
//! hand-made data.) Party members have no affinities: armor and accessory resistances are not modelled.

use std::collections::BTreeMap;

use crate::kernel::{ItemRecord, Kernel, ITEM_COUNT};
use crate::scene::{AffinityRate, AttackRecord, SceneAttack};
pub use crate::status::{StatusEffect, StatusMode};

/// Element bit masks of an attack record (`<https://wiki.ffrtt.ru/index.php/FF7/Battle/Elemental_Data>`).
pub mod element {
    pub const FIRE: u16 = 0x0001;
    pub const ICE: u16 = 0x0002;
    pub const BOLT: u16 = 0x0004;
    pub const EARTH: u16 = 0x0008;
    pub const POISON: u16 = 0x0010;
    pub const GRAVITY: u16 = 0x0020;
    pub const WATER: u16 = 0x0040;
    pub const WIND: u16 = 0x0080;
    pub const HOLY: u16 = 0x0100;
    /// Healing: Cure, Life, potions. Undead enemies are weak to it.
    pub const RESTORATIVE: u16 = 0x0200;
    pub const CUT: u16 = 0x0400;
    pub const HIT: u16 = 0x0800;
    pub const PUNCH: u16 = 0x1000;
    pub const SHOOT: u16 = 0x2000;
    pub const SHOUT: u16 = 0x4000;
}

/// The special flag bits that matter (active low: the effect is on when the bit is CLEAR).
const FLAG_HITS_MP: u16 = 0x0001;
const FLAG_HITS_DEAD: u16 = 0x0100;
const FLAG_IGNORES_DEFENSE: u16 = 0x0400;
/// Targeting bits.
/// The cursor starts on the enemies (clear: on the actor's own side).
const TARGET_ENEMY_ROW: u8 = 0x02;
const TARGET_MULTI_DEFAULT: u8 = 0x04;
const TARGET_TOGGLE_MULTI: u8 = 0x08;
const TARGET_SHORT_RANGE: u8 = 0x20;
const TARGET_RANDOM: u8 = 0x80;

/// Physical actions use Strength and Defense, magical ones Magic and Magic defense.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionKind {
    Physical,
    Magical,
}

/// How many an action can be aimed at: the targeting bits `0x04`, `0x08` and `0x80` of its record (see the
/// module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Aim {
    /// One selected target (bit `0x04` clear): Potion, Comet, the Attack command.
    One,
    /// One target, or the whole side (`0x04` and `0x08`: FF7's "All Tar/1 Tar"): Cure, Fire. A party member
    /// aims a spell of this kind at a whole side only while it has an All materia use left
    /// ([`crate::rules::Combatant::all_left`]); an item or a limit of this kind needs nothing.
    OneOrSide,
    /// Only ever the whole side (`0x04` without `0x08`: "All Tar (NS)", No Split): Ultima, Megalixir,
    /// Healing Wind. It needs no All materia and cannot be aimed at one target.
    Side,
    /// A random target (`0x80`): neither a command nor a menu can ask for it, so a spell or an item of this
    /// kind is left out (an enemy's own attack of this kind is played as it always was, on one random
    /// party member).
    Random,
}

impl Aim {
    /// The aim a record's targeting bits give.
    pub fn from_target_flags(flags: u8) -> Self {
        if flags & TARGET_RANDOM != 0 {
            Aim::Random
        } else if flags & TARGET_MULTI_DEFAULT == 0 {
            Aim::One
        } else if flags & TARGET_TOGGLE_MULTI != 0 {
            Aim::OneOrSide
        } else {
            Aim::Side
        }
    }

    /// Whether a command may aim an action of this kind at one target.
    pub fn allows_one(self) -> bool {
        matches!(self, Aim::One | Aim::OneOrSide)
    }

    /// Whether a command may aim an action of this kind at a whole side.
    pub fn allows_side(self) -> bool {
        matches!(self, Aim::OneOrSide | Aim::Side)
    }
}

/// How an action's amount is computed (the low nybble of the damage-calculation byte, see the module
/// docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Formula {
    /// `0x?1`: the standard attack formula.
    Standard,
    /// `0x?2`: the standard spell formula.
    Spell,
    /// `0x?3`: a fraction of the target's current HP (Demi).
    CurrentHp,
    /// `0x?4`: a fraction of the target's max HP (Life, Full Cure, X-Potion).
    MaxHp,
    /// `0x?5`: Cure's formula.
    Restore,
    /// `0x?6`: a flat amount (Potion, Hi-Potion, Ether).
    Flat,
    /// `0x?7`: a fixed amount reduced by defense (Grenade).
    Fixed,
    /// `0x?8`: everything (Elixir).
    Full,
    /// No damage or recovery at all (a Power of 0, the byte `0x50` or `0xFF`): the action only has its status
    /// ([`ActionSpec::status`]). Never in a damage-calculation byte's low nybble: it is what a record without a
    /// formula decodes to when it has a status.
    None,
}

impl Formula {
    fn from_code(low: u8) -> Option<Self> {
        Some(match low {
            1 => Formula::Standard,
            2 => Formula::Spell,
            3 => Formula::CurrentHp,
            4 => Formula::MaxHp,
            5 => Formula::Restore,
            6 => Formula::Flat,
            7 => Formula::Fixed,
            8 => Formula::Full,
            _ => return None,
        })
    }

    /// Whether the random variance `(3841 + 0..=255)/4096` follows the formula (and so costs a roll).
    pub fn has_variance(self) -> bool {
        matches!(
            self,
            Formula::Standard | Formula::Spell | Formula::Restore | Formula::Fixed
        )
    }
}

/// An action as the rules play it: decoded from an attack or item record, or built by hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActionSpec {
    pub kind: ActionKind,
    pub formula: Formula,
    /// The record's Power (the Attack command's own Power for an attack with no record).
    pub power: u32,
    /// "Attack %": the hit rate in percent (255 for what cannot miss, and for every item).
    pub accuracy: u32,
    /// Whether the action rolls to hit at all (see the table in the module docs).
    pub rolls_accuracy: bool,
    /// Whether a hit can be a critical hit.
    pub can_crit: bool,
    /// The element mask ([`element`]).
    pub element_mask: u16,
    /// Hits MP instead of HP (special flag `0x0001` cleared): Ether.
    pub hits_mp: bool,
    /// Only a dead target is valid (special flag `0x0100` cleared): Life, Phoenix Down.
    pub hits_dead: bool,
    /// Defense is left out of the damage (special flag `0x0400` cleared).
    pub ignores_defense: bool,
    /// Halved against the back row (targeting bit `0x20`; the enemy attack command always is).
    pub short_range: bool,
    /// MP the action costs.
    pub mp_cost: u16,
    /// How many it can be aimed at (see [`Aim`]).
    pub aim: Aim,
    /// What it does to the statuses of its targets, if anything (see the module docs, "Statuses").
    pub status: Option<StatusEffect>,
    /// The Attack command itself, the weapon attack (Darkness halves its hit rate, a party member's only: see
    /// [`crate::status`]). Set by [`ActionSpec::basic_attack`] and nothing else.
    pub weapon_attack: bool,
    /// For a status-only action ([`Formula::None`]): it aims at the actor's own side (a buff or a cure, the
    /// record's targeting bit `0x02` clear) and not at the other one. Meaningless for any other action, whose side
    /// is [`ActionSpec::heals`]'s answer.
    pub on_allies: bool,
}

impl ActionSpec {
    /// A plain physical attack of `power` and `accuracy`: what the Attack command is.
    pub fn basic_attack(power: u32, accuracy: u32, short_range: bool) -> Self {
        Self {
            kind: ActionKind::Physical,
            formula: Formula::Standard,
            power,
            accuracy,
            rolls_accuracy: true,
            can_crit: true,
            element_mask: 0,
            hits_mp: false,
            hits_dead: false,
            ignores_defense: false,
            short_range,
            mp_cost: 0,
            aim: Aim::One,
            status: None,
            weapon_attack: true,
            on_allies: false,
        }
    }

    /// The same action aimed another way (`ActionSpec::cure_spell(..).with_aim(Aim::Side)`).
    pub fn with_aim(self, aim: Aim) -> Self {
        Self { aim, ..self }
    }

    /// Whether the action's damage or healing is multiplied by 2/3 when it hits more than one target (see
    /// the module docs, "Hitting a whole side"): the physical formula and Cure's always, the spell formula
    /// only for a spell that can be toggled between one target and all of them ([`Aim::OneOrSide`]), and
    /// no other formula. A caller that hits one target never splits, whatever this says.
    pub fn splits(&self) -> bool {
        match self.formula {
            Formula::Standard | Formula::Restore => true,
            Formula::Spell => self.aim == Aim::OneOrSide,
            Formula::CurrentHp | Formula::MaxHp | Formula::Flat | Formula::Fixed | Formula::Full | Formula::None => false,
        }
    }

    /// A hand-made magical damage spell (Fire's kind: formula 2, an accuracy roll, one target or a whole
    /// side, like the record's `0x0F`): for rule sets that have no game files (`SyntheticRules`) and for
    /// tests.
    pub fn damage_spell(power: u32, accuracy: u32, element_mask: u16, mp_cost: u16) -> Self {
        Self {
            kind: ActionKind::Magical,
            formula: Formula::Spell,
            power,
            accuracy,
            rolls_accuracy: true,
            can_crit: false,
            element_mask,
            hits_mp: false,
            hits_dead: false,
            ignores_defense: false,
            short_range: false,
            mp_cost,
            aim: Aim::OneOrSide,
            status: None,
            weapon_attack: false,
            on_allies: false,
        }
    }

    /// A hand-made restoring spell (Cure's kind: formula 5, restorative, aimed at an ally or the whole
    /// party, like the record's `0x0D`).
    pub fn cure_spell(power: u32, mp_cost: u16) -> Self {
        Self {
            formula: Formula::Restore,
            element_mask: element::RESTORATIVE,
            ..Self::damage_spell(power, 255, 0, mp_cost)
        }
    }

    /// A hand-made revival (Life's kind: a fraction `power / 32` of max HP, only for the dead).
    pub fn revive_spell(power: u32, mp_cost: u16) -> Self {
        Self {
            formula: Formula::MaxHp,
            hits_dead: true,
            ..Self::cure_spell(power, mp_cost)
        }
    }

    /// A hand-made damage item (Grenade's kind: `(512 - Defense) * power / 32` with variance, never misses,
    /// no critical hit, one enemy; [`ActionSpec::with_aim`] makes it Shrapnel's, which hits every enemy).
    pub fn damage_item(power: u32, element_mask: u16) -> Self {
        Self {
            kind: ActionKind::Physical,
            formula: Formula::Fixed,
            rolls_accuracy: false,
            can_crit: false,
            aim: Aim::One,
            ..Self::damage_spell(power, 255, element_mask, 0)
        }
    }

    /// A hand-made flat restoring item (Potion's kind: `20 * power` HP, or MP when `mp` is set; one ally).
    pub fn flat_restore_item(power: u32, mp: bool) -> Self {
        Self {
            formula: Formula::Flat,
            hits_mp: mp,
            rolls_accuracy: false,
            aim: Aim::One,
            ..Self::cure_spell(power, 0)
        }
    }

    /// A hand-made item that restores everything, HP and MP (Elixir's kind, one ally;
    /// [`ActionSpec::with_aim`]`(`[`Aim::Side`]`)` makes it Megalixir's, which restores the whole party).
    pub fn full_restore_item() -> Self {
        Self {
            kind: ActionKind::Physical,
            formula: Formula::Full,
            power: 32,
            rolls_accuracy: false,
            aim: Aim::One,
            ..Self::cure_spell(32, 0)
        }
    }

    /// A limit break that strikes one enemy: the standard physical formula at `power` (16 is a plain
    /// attack), which never misses, cannot be a critical hit, costs no MP and ignores the rows (see
    /// [`crate::limit`]).
    pub fn limit_strike(power: u32) -> Self {
        Self {
            kind: ActionKind::Physical,
            formula: Formula::Standard,
            power,
            accuracy: 255,
            rolls_accuracy: false,
            can_crit: false,
            element_mask: 0,
            hits_mp: false,
            hits_dead: false,
            ignores_defense: false,
            short_range: false,
            mp_cost: 0,
            aim: Aim::One,
            status: None,
            weapon_attack: false,
            on_allies: false,
        }
    }

    /// A limit break that casts at one enemy: the spell formula at `power` and with `element_mask`,
    /// otherwise like [`ActionSpec::limit_strike`] (Vincent's Galian Beast).
    pub fn limit_blast(power: u32, element_mask: u16) -> Self {
        Self { kind: ActionKind::Magical, formula: Formula::Spell, element_mask, ..Self::limit_strike(power) }
    }

    /// A limit break that heals the whole party, each ally for `power / 32` of its max HP (16 is half;
    /// Healing Wind, whose record says "All Al (NS)": every ally at once, at full strength).
    pub fn limit_heal(power: u32) -> Self {
        Self {
            kind: ActionKind::Magical,
            formula: Formula::MaxHp,
            element_mask: element::RESTORATIVE,
            aim: Aim::Side,
            ..Self::limit_strike(power)
        }
    }

    /// A hand-made action that only has a status (Sleep's kind: magical, an accuracy roll of `accuracy` unless it
    /// is aimed at allies; `on_allies` says which side it aims at, [`ActionSpec::on_allies`]), one target or a
    /// whole side like the records' `0x0D` and `0x0F`.
    pub fn status_spell(effect: StatusEffect, accuracy: u32, mp_cost: u16, on_allies: bool) -> Self {
        Self {
            formula: Formula::None,
            power: 0,
            status: Some(effect),
            on_allies,
            ..Self::damage_spell(0, accuracy, 0, mp_cost)
        }
    }

    /// A hand-made item that only has a status (an Antidote's kind: it never misses, one target; `on_allies` as for
    /// [`ActionSpec::status_spell`]; [`ActionSpec::with_aim`] makes it hit a whole side).
    pub fn status_item(effect: StatusEffect, on_allies: bool) -> Self {
        Self { rolls_accuracy: false, aim: Aim::One, ..Self::status_spell(effect, 255, 0, on_allies) }
    }

    /// The same action with a status effect added (Bio's kind: damage and then a chance of Poison).
    pub fn with_status(self, effect: StatusEffect) -> Self {
        Self { status: Some(effect), ..self }
    }

    /// Whether the action aims at the actor's own side: a restoring action does ([`ActionSpec::heals`]), a
    /// damaging one does not, and a status-only one says so itself ([`ActionSpec::on_allies`]).
    pub fn aims_at_allies(&self) -> bool {
        if self.formula == Formula::None {
            self.on_allies
        } else {
            self.heals()
        }
    }

    /// Decode an attack record (a spell of the kernel's attack table or an enemy's attack of
    /// `scene.bin`), or `None` if the rules do not model it (see the module docs).
    pub fn from_attack(rec: &AttackRecord) -> Option<Self> {
        Self::decode(
            rec.damage_calc,
            u32::from(rec.power),
            u32::from(rec.accuracy),
            rec.mp_cost,
            rec.target_flags,
            rec.element_mask,
            rec.special_flags,
            StatusEffect::from_record(rec.status_mask, rec.status_chance_mode),
        )
    }

    /// Decode an item record, or `None` if the rules do not model it. Items have no accuracy byte and
    /// are never missed.
    pub fn from_item(rec: &ItemRecord) -> Option<Self> {
        let mut spec = Self::decode(
            rec.damage_calc,
            u32::from(rec.power),
            255,
            0,
            rec.target_flags,
            rec.element_mask,
            rec.special_flags,
            StatusEffect::from_record(rec.status_mask, rec.status_chance_mode),
        )?;
        // Items that do something else as well (Stardust, Smoke Bomb, Hero Drink) are not modelled.
        if rec.extra_effect != 0xFF {
            return None;
        }
        spec.rolls_accuracy = false;
        Some(spec)
    }

    #[allow(clippy::too_many_arguments)] // the fields of a record, in the order of its layout
    fn decode(
        calc: u8,
        power: u32,
        accuracy: u32,
        mp_cost: u16,
        target_flags: u8,
        element_mask: u16,
        special_flags: u16,
        status: Option<StatusEffect>,
    ) -> Option<Self> {
        let (kind, rolls_accuracy, can_crit) = match calc >> 4 {
            0x0 | 0x3 => (ActionKind::Physical, false, false),
            0x1 | 0xA => (ActionKind::Physical, true, true),
            0xB => (ActionKind::Physical, true, false),
            0x2 => (ActionKind::Magical, true, false),
            0x4 | 0x5 => (ActionKind::Magical, false, false),
            // `0xFF`: no damage part at all, so only a status can make it an action (below).
            0xF if status.is_some() => (ActionKind::Magical, true, false),
            _ => return None,
        };
        let aim = Aim::from_target_flags(target_flags);
        let formula = match Formula::from_code(calc & 0x0F) {
            Some(formula) if power > 0 || formula == Formula::Full => formula,
            // No formula, or no Power: nothing to deal or restore. With a status it is a status-only action.
            _ => {
                status?;
                return Some(Self {
                    kind,
                    formula: Formula::None,
                    power: 0,
                    accuracy,
                    rolls_accuracy,
                    can_crit: false,
                    element_mask,
                    hits_mp: false,
                    hits_dead: false,
                    ignores_defense: false,
                    short_range: false,
                    mp_cost,
                    aim,
                    status,
                    weapon_attack: false,
                    on_allies: target_flags & TARGET_ENEMY_ROW == 0,
                });
            }
        };
        // "Everything" is a recovery: without the restorative element it would mean nothing.
        if formula == Formula::Full && element_mask & element::RESTORATIVE == 0 {
            return None;
        }
        Some(Self {
            kind,
            formula,
            power,
            accuracy,
            rolls_accuracy,
            can_crit,
            element_mask,
            hits_mp: special_flags & FLAG_HITS_MP == 0,
            hits_dead: special_flags & FLAG_HITS_DEAD == 0,
            ignores_defense: special_flags & FLAG_IGNORES_DEFENSE == 0,
            short_range: target_flags & TARGET_SHORT_RANGE != 0,
            mp_cost,
            aim,
            status,
            weapon_attack: false,
            on_allies: false,
        })
    }

    /// Whether the action restores (HP, MP or a revival) instead of damaging: a Restorative element
    /// with formula 4, 5, 6 or 8. Such an action is aimed at an ally.
    pub fn heals(&self) -> bool {
        self.element_mask & element::RESTORATIVE != 0
            && matches!(
                self.formula,
                Formula::MaxHp | Formula::Restore | Formula::Flat | Formula::Full
            )
    }
}

/// A spell a party member can cast: the kernel's attack index and what it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spell {
    /// The attack index in the kernel's attack data (section 1): Cure 0, Cure2 1, Cure3 2, Life 7,
    /// Fire 27, Ice 30, Bolt 33, ...; also the index of its name in "Names - magic".
    pub index: u16,
    pub spec: ActionSpec,
}

impl Spell {
    /// A hand-made spell (see the `ActionSpec` constructors); a spell from the kernel is
    /// [`Spell::from_attack`].
    pub fn new(index: u16, spec: ActionSpec) -> Self {
        Self { index, spec }
    }

    /// The spell for attack record `index`, or `None` if it is not one the rules can cast: it must be
    /// magical, have a Power, not aim at a random target, and have no extra effect. A spell that hits a
    /// whole side (Ultima) is one.
    pub fn from_attack(index: u16, rec: &AttackRecord) -> Option<Self> {
        let spec = ActionSpec::from_attack(rec)?;
        let castable =
            spec.kind == ActionKind::Magical && spec.aim != Aim::Random && rec.extra_effect == 0xFF;
        castable.then_some(Self { index, spec })
    }

    /// What a menu needs to know about the spell.
    pub fn info(&self) -> SpellInfo {
        SpellInfo {
            spell: self.index,
            mp_cost: self.spec.mp_cost,
            targets_allies: self.spec.aims_at_allies(),
            aim: self.spec.aim,
        }
    }
}

/// What a battle menu shows of a spell (see `BattleState::spells_for`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpellInfo {
    /// The kernel attack index ([`Spell::index`]); its name is in the machine's own text.
    pub spell: u16,
    pub mp_cost: u16,
    /// Aimed at the party (a restoring spell) instead of the enemies.
    pub targets_allies: bool,
    /// One target, or a whole side too, or a whole side only (see [`Aim`]; a spell of
    /// [`Aim::OneOrSide`] hits a whole side only while its caster has an All use left).
    pub aim: Aim,
}

/// One of the attacks an enemy picks from: a damaging attack of its scene.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnemyAction {
    /// The attack id of the scene (`ActionRef::EnemyAttack`).
    pub id: u16,
    pub spec: ActionSpec,
}

impl EnemyAction {
    /// The enemy action for a scene attack, or `None` if it does not damage in a way the rules model
    /// (status-only, scripted, healing). The enemy attack command is always row-sensitive, and an enemy
    /// aims by its own rules (one random party member here), so the record's targeting does not choose
    /// the target (it stays in [`ActionSpec::aim`] for the rules that decide whom an attack hits).
    ///
    /// A damaging attack that also has a status keeps it ([`ActionSpec::status`]: the stinging attacks of the
    /// scenes inflict theirs). An attack with **only** a status is left out of the pool the random AI picks from,
    /// as it always was: an enemy's script can still queue it ([`EnemyAction::status_from_scene`]).
    pub fn from_scene(attack: &SceneAttack) -> Option<Self> {
        let mut spec = ActionSpec::from_attack(&attack.record)?;
        if spec.heals() || spec.formula == Formula::None {
            return None;
        }
        spec.short_range = spec.kind == ActionKind::Physical;
        Some(Self {
            id: attack.id,
            spec,
        })
    }

    /// The enemy action for a scene attack that has **only** a status (a curse or a buff: [`Formula::None`]), or
    /// `None` for any other attack. The random AI never picks one (`Combatant::status_actions`, which holds them, is
    /// not its pool); an enemy's script that queues it has it played ([`ActionSpec::status`] on whomever the
    /// script aims it at). An attack with no damage, no healing and no status the rules play (an animation: the
    /// Guard Scorpion's tail going up) is not an action at all.
    pub fn status_from_scene(attack: &SceneAttack) -> Option<Self> {
        let spec = ActionSpec::from_attack(&attack.record)?;
        (spec.formula == Formula::None).then_some(Self {
            id: attack.id,
            spec,
        })
    }
}

/// How a target answers one element: the element's index (`0..=15`, bit `1 << element` of an
/// action's mask) and the rate code of the enemy record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ElementAffinity {
    pub element: u8,
    pub rate: AffinityRate,
}

/// The affinity that applies to an action with element mask `mask`, if any: the first of Absorb, Full
/// heal, Nullify, Death, Half, Double (see the module docs) that one of its elements has.
pub fn affinity_response(affinities: &[ElementAffinity], mask: u16) -> Option<AffinityRate> {
    const ORDER: [AffinityRate; 6] = [
        AffinityRate::Absorb,
        AffinityRate::FullHeal,
        AffinityRate::Nullify,
        AffinityRate::Death,
        AffinityRate::Half,
        AffinityRate::Double,
    ];
    ORDER.into_iter().find(|rate| {
        affinities
            .iter()
            .any(|a| a.rate == *rate && a.element < 16 && mask & (1u16 << a.element) != 0)
    })
}

/// A damage multiplier from an element: double for a weakness, half for a resistance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Elemental {
    #[default]
    Normal,
    Double,
    Half,
}

impl Elemental {
    /// The multiplier an affinity gives (`Normal` for the ones that do not scale damage).
    pub fn of(response: Option<AffinityRate>) -> Self {
        match response {
            Some(AffinityRate::Double) => Elemental::Double,
            Some(AffinityRate::Half) => Elemental::Half,
            _ => Elemental::Normal,
        }
    }
}

/// What an item does (derived from its record, see [`ItemInfo`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    /// Restores HP (Potion, Hi-Potion, X-Potion).
    RestoreHp,
    /// Restores MP (Ether, Turbo Ether).
    RestoreMp,
    /// Restores HP and MP (Elixir).
    RestoreAll,
    /// Brings a dead ally back (Phoenix Down).
    Revive,
    /// Damages one enemy (Grenade).
    Damage,
    /// Only changes statuses: an Antidote, an Echo Screen or a Remedy cures them, a Speed Drink inflicts a
    /// buff and a Dazers inflicts a curse (whom it aims at is [`ItemInfo::targets_allies`]).
    Status,
}

/// An item the rules can use in battle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemData {
    pub spec: ActionSpec,
}

impl ItemData {
    /// A hand-made item (see the `ActionSpec` constructors).
    pub fn new(spec: ActionSpec) -> Self {
        Self { spec }
    }

    /// The item for a kernel record, or `None` if it cannot be used in battle or is not modelled
    /// (status cures, stat boosts, escape items, items that aim at a random target). An item that hits a
    /// whole side (Megalixir, Shrapnel) is one.
    pub fn from_record(rec: &ItemRecord) -> Option<Self> {
        if !rec.usable_in_battle() {
            return None;
        }
        let spec = ActionSpec::from_item(rec)?;
        (spec.aim != Aim::Random).then_some(Self { spec })
    }

    pub fn kind(&self) -> ItemKind {
        let spec = &self.spec;
        if spec.formula == Formula::None {
            ItemKind::Status
        } else if !spec.heals() {
            ItemKind::Damage
        } else if spec.hits_dead {
            ItemKind::Revive
        } else if spec.formula == Formula::Full {
            ItemKind::RestoreAll
        } else if spec.hits_mp {
            ItemKind::RestoreMp
        } else {
            ItemKind::RestoreHp
        }
    }

    /// What a menu needs to know about item `item`.
    pub fn info(&self, item: u16) -> ItemInfo {
        let kind = self.kind();
        ItemInfo {
            item,
            kind,
            targets_allies: self.spec.aims_at_allies(),
            revives: kind == ItemKind::Revive,
            aim: self.spec.aim,
        }
    }
}

/// What a battle menu shows of an item (see `BattleState::item_info`). The inventory (how many the
/// account has) is NOT part of the engine: the server checks it and the client learns it from the
/// server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemInfo {
    /// The kernel item index; its name is in the machine's own text.
    pub item: u16,
    pub kind: ItemKind,
    /// Aimed at the party: everything but a damage item and a curse (a Dazers, a Hourglass: [`ActionSpec::aims_at_allies`]).
    pub targets_allies: bool,
    /// Only a dead ally is a valid target (Phoenix Down).
    pub revives: bool,
    /// One target, a whole side too (free to choose: an item needs no All materia), or a whole side only
    /// (Megalixir, Shrapnel).
    pub aim: Aim,
}

/// The items the rules can use in battle, by item index. Built once from the kernel and shared by
/// every battle ([`crate::rules::BattleState::with_items`]); it depends on the kernel only, so the
/// battle data fingerprint covers it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ItemTable {
    items: BTreeMap<u16, ItemData>,
}

impl ItemTable {
    /// A table of exactly these items (tests and hand-made rule sets).
    pub fn new(items: impl IntoIterator<Item = (u16, ItemData)>) -> Self {
        Self {
            items: items.into_iter().collect(),
        }
    }

    /// Every record of the kernel's item table that the rules can use in battle.
    pub fn from_kernel(kernel: &Kernel) -> Self {
        let items = (0..ITEM_COUNT as u16)
            .filter_map(|i| Some((i, ItemData::from_record(&kernel.item(i)?)?)))
            .collect();
        Self { items }
    }

    pub fn get(&self, item: u16) -> Option<&ItemData> {
        self.items.get(&item)
    }

    pub fn info(&self, item: u16) -> Option<ItemInfo> {
        self.get(item).map(|d| d.info(item))
    }

    /// The item indexes the table holds, ascending.
    pub fn indexes(&self) -> impl Iterator<Item = u16> + '_ {
        self.items.keys().copied()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::Status;
    use crate::testutil::{attack_record, synthetic_kernel_bytes};

    fn attack(
        calc: u8,
        power: u8,
        accuracy: u8,
        target: u8,
        element: u16,
        flags: u16,
    ) -> AttackRecord {
        attack_record(calc, power, accuracy, target, element, flags, 7)
    }

    #[test]
    fn the_high_nybble_says_how_it_can_miss_and_the_low_one_what_the_formula_is() {
        // The restorative element, so that formula 8 (a recovery) has a meaning.
        let spec = |calc| {
            ActionSpec::from_attack(&attack(calc, 10, 100, 0x03, element::RESTORATIVE, 0xFFFF))
        };
        // Physical, accuracy roll, can crit: the enemy's plain attack.
        let basic = spec(0x11).unwrap();
        assert_eq!(
            (basic.kind, basic.formula),
            (ActionKind::Physical, Formula::Standard)
        );
        assert!(basic.rolls_accuracy && basic.can_crit);
        // 0x01 and 0x31 never miss and never crit; 0xA1 rolls and crits; 0xB1 rolls without crit.
        for calc in [0x01, 0x31] {
            let s = spec(calc).unwrap();
            assert!(!s.rolls_accuracy && !s.can_crit, "{calc:#x}");
        }
        let a = spec(0xA1).unwrap();
        assert!(a.rolls_accuracy && a.can_crit);
        let b = spec(0xB1).unwrap();
        assert!(b.rolls_accuracy && !b.can_crit);
        // Magic: 0x2X rolls, 0x4X / 0x5X never miss; magic never crits.
        let fire = spec(0x22).unwrap();
        assert_eq!(
            (fire.kind, fire.formula),
            (ActionKind::Magical, Formula::Spell)
        );
        assert!(fire.rolls_accuracy && !fire.can_crit);
        assert!(!spec(0x42).unwrap().rolls_accuracy && !spec(0x52).unwrap().rolls_accuracy);
        // Every low nybble from 1 to 8 is a formula, and the others are not.
        let formulas: Vec<Formula> = (1..=8)
            .map(|low| spec(0x20 | low).unwrap().formula)
            .collect();
        assert_eq!(
            formulas,
            [
                Formula::Standard,
                Formula::Spell,
                Formula::CurrentHp,
                Formula::MaxHp,
                Formula::Restore,
                Formula::Flat,
                Formula::Fixed,
                Formula::Full
            ]
        );
        for calc in [
            0x20, 0x29, 0x2F, 0x00, 0x50, 0x60, 0x61, 0x69, 0x71, 0x83, 0x91, 0xC1, 0xFF,
        ] {
            assert!(spec(calc).is_none(), "{calc:#x} is not modelled");
        }
        // Variance follows formulas 1, 2, 5 and 7 only.
        let with: Vec<bool> = (1..=8)
            .map(|low| Formula::from_code(low).unwrap().has_variance())
            .collect();
        assert_eq!(with, [true, true, false, false, true, false, true, false]);
    }

    #[test]
    fn a_power_of_zero_is_a_status_and_is_left_out_except_for_the_full_restore() {
        assert!(
            ActionSpec::from_attack(&attack(0x22, 0, 100, 0x0D, 0, 0xFDFF)).is_none(),
            "Regen: status only"
        );
        assert!(ActionSpec::from_attack(&attack(0x11, 0, 100, 0x03, 0, 0xFFFF)).is_none());
        let elixir =
            ActionSpec::from_attack(&attack(0x08, 0, 255, 0x01, element::RESTORATIVE, 0xFFFF))
                .unwrap();
        assert_eq!(elixir.formula, Formula::Full);
        assert!(elixir.heals());
        // "Everything" without the restorative element means nothing.
        assert!(ActionSpec::from_attack(&attack(0x08, 0, 255, 0x01, 0, 0xFFFF)).is_none());
    }

    #[test]
    fn the_flags_are_active_low_and_the_targeting_bits_decide_how_it_can_be_aimed() {
        // Cure (0x25, Power 5): restorative, hits HP, the living, one target or the whole side (0x0D:
        // selection, whole side by default, toggle to one).
        let cure =
            ActionSpec::from_attack(&attack(0x25, 5, 255, 0x0D, element::RESTORATIVE, 0xFDFF))
                .unwrap();
        assert!(
            cure.heals()
                && !cure.hits_mp
                && !cure.hits_dead
                && cure.aim == Aim::OneOrSide
                && !cure.ignores_defense
        );
        // Life: the dead only (bit 0x0100 cleared).
        let life =
            ActionSpec::from_attack(&attack(0x24, 8, 255, 0x0D, element::RESTORATIVE, 0xFCFD))
                .unwrap();
        assert!(life.hits_dead && life.heals());
        // Ether: hits MP (bit 0x0001 cleared).
        let ether =
            ActionSpec::from_attack(&attack(0x26, 5, 255, 0x01, element::RESTORATIVE, 0xFFFE))
                .unwrap();
        assert!(ether.hits_mp && !ether.hits_dead);
        // Piercing: bit 0x0400 cleared.
        assert!(
            ActionSpec::from_attack(&attack(0x22, 5, 100, 0x0F, 0, 0xFBFF))
                .unwrap()
                .ignores_defense
        );
        // Ultima (0x07): a whole side with no toggle ("All Tar (NS)"); random targets (0x80) are a kind of
        // their own, whatever else the bits say.
        let aim_of = |flags| ActionSpec::from_attack(&attack(0x22, 5, 100, flags, 0, 0xFFFF)).unwrap().aim;
        assert_eq!(aim_of(0x07), Aim::Side);
        assert_eq!(aim_of(0x05), Aim::Side, "Megalixir's bits");
        assert_eq!(aim_of(0x81), Aim::Random);
        assert_eq!(aim_of(0x87), Aim::Random, "multiple and random is still random");
        // Fire (0x0F: toggle) and Comet (0x03: one target) can be aimed; 0x20 is short range.
        assert_eq!(aim_of(0x0F), Aim::OneOrSide);
        assert_eq!(aim_of(0x0D), Aim::OneOrSide);
        assert_eq!(aim_of(0x03), Aim::One);
        assert_eq!(aim_of(0x01), Aim::One);
        assert_eq!(aim_of(0x08), Aim::One, "a toggle without the default is no way to hit everyone");
        assert_eq!(aim_of(0x17), Aim::Side, "a summon's bits: the one-row bit does not matter here");
        assert!(
            ActionSpec::from_attack(&attack(0x11, 5, 100, 0x23, 0, 0xFFFF))
                .unwrap()
                .short_range
        );
        // Healing needs the restorative element AND a healing formula: Gravity-like MaxHp damage hurts.
        assert!(
            !ActionSpec::from_attack(&attack(0x24, 8, 100, 0x0F, element::GRAVITY, 0xFFFF))
                .unwrap()
                .heals()
        );
        assert!(!ActionSpec::from_attack(&attack(
            0x22,
            8,
            100,
            0x0F,
            element::RESTORATIVE,
            0xFFFF
        ))
        .unwrap()
        .heals());
    }

    #[test]
    fn spells_must_be_magical_aimable_and_without_side_effects() {
        let fire = attack(0x22, 8, 100, 0x0F, element::FIRE, 0xFDFF);
        let spell = Spell::from_attack(27, &fire).unwrap();
        assert_eq!(
            spell.info(),
            SpellInfo {
                spell: 27,
                mp_cost: 7,
                targets_allies: false,
                aim: Aim::OneOrSide
            }
        );
        let cure = Spell::from_attack(0, &attack(0x25, 5, 255, 0x0D, element::RESTORATIVE, 0xFDFF))
            .unwrap();
        assert!(cure.info().targets_allies);
        // A whole-side spell (Ultima) is one, aimed at a whole side only.
        let ultima = Spell::from_attack(52, &attack(0x22, 105, 100, 0x07, 0, 0xFFFD)).unwrap();
        assert_eq!(ultima.info().aim, Aim::Side);
        // A physical record, a random one, a status and a spell with an extra effect are not spells.
        assert!(Spell::from_attack(3, &attack(0x11, 24, 90, 0x03, 0, 0xFFFF)).is_none());
        assert!(Spell::from_attack(46, &attack(0x22, 30, 100, 0x87, 0, 0xFFFF)).is_none());
        assert!(Spell::from_attack(3, &attack(0x22, 0, 255, 0x0D, 0, 0xFDFF)).is_none());
        let mut odd = fire;
        odd.extra_effect = 3;
        assert!(Spell::from_attack(27, &odd).is_none());
    }

    #[test]
    fn an_enemy_attack_ignores_its_aim_and_is_row_sensitive_when_physical() {
        let scene_attack = |id, record| SceneAttack {
            id,
            name: "x".into(),
            record,
        };
        let bite = EnemyAction::from_scene(&scene_attack(
            0x0301,
            attack(0x11, 16, 100, 0x03, element::HIT, 0xFFFF),
        ))
        .unwrap();
        assert!(bite.spec.short_range && bite.id == 0x0301);
        let ultima =
            EnemyAction::from_scene(&scene_attack(9, attack(0x22, 105, 100, 0x07, 0, 0xFFFF)))
                .unwrap();
        assert!(
            ultima.spec.aim == Aim::Side && !ultima.spec.short_range,
            "an enemy picks its own target (the record's aim is kept); magic ignores rows"
        );
        // Healing and status attacks are not something an enemy is modelled to do.
        assert!(EnemyAction::from_scene(&scene_attack(
            1,
            attack(0x25, 5, 255, 0x0D, element::RESTORATIVE, 0xFDFF)
        ))
        .is_none());
        assert!(
            EnemyAction::from_scene(&scene_attack(2, attack(0x22, 0, 100, 0x03, 0, 0xFFFF)))
                .is_none()
        );
        assert!(
            EnemyAction::from_scene(&scene_attack(3, attack(0x00, 0, 100, 0x03, 0, 0xFFFF)))
                .is_none()
        );
        assert!(
            EnemyAction::from_scene(&scene_attack(4, attack(0x61, 0, 100, 0x03, 0, 0xFFFF)))
                .is_none()
        );
    }

    #[test]
    fn affinities_answer_by_element_with_a_fixed_precedence() {
        let aff = |element, rate| ElementAffinity { element, rate };
        let table = [
            aff(0, AffinityRate::Double),
            aff(1, AffinityRate::Half),
            aff(2, AffinityRate::Nullify),
            aff(3, AffinityRate::Absorb),
            aff(4, AffinityRate::Death),
            aff(5, AffinityRate::FullHeal),
        ];
        assert_eq!(
            affinity_response(&table, element::FIRE),
            Some(AffinityRate::Double)
        );
        assert_eq!(
            affinity_response(&table, element::ICE),
            Some(AffinityRate::Half)
        );
        assert_eq!(
            affinity_response(&table, element::BOLT),
            Some(AffinityRate::Nullify)
        );
        assert_eq!(
            affinity_response(&table, element::EARTH),
            Some(AffinityRate::Absorb)
        );
        assert_eq!(
            affinity_response(&table, element::POISON),
            Some(AffinityRate::Death)
        );
        assert_eq!(
            affinity_response(&table, element::GRAVITY),
            Some(AffinityRate::FullHeal)
        );
        assert_eq!(
            affinity_response(&table, element::WATER),
            None,
            "no entry: normal damage"
        );
        assert_eq!(
            affinity_response(&table, 0),
            None,
            "no element: nothing applies"
        );
        // Several elements: the strongest answer in the order Absorb, Full heal, Nullify, Death, Half, Double.
        assert_eq!(
            affinity_response(&table, element::FIRE | element::ICE),
            Some(AffinityRate::Half)
        );
        assert_eq!(
            affinity_response(&table, element::FIRE | element::BOLT),
            Some(AffinityRate::Nullify)
        );
        assert_eq!(
            affinity_response(&table, element::BOLT | element::EARTH | element::GRAVITY),
            Some(AffinityRate::Absorb)
        );
        assert_eq!(
            affinity_response(&table, element::POISON | element::ICE),
            Some(AffinityRate::Death)
        );
        // Codes that are not elements (a status code, an unknown rate) never apply or scale damage.
        let odd = [
            aff(0x22, AffinityRate::Double),
            aff(0, AffinityRate::Other(3)),
            aff(200, AffinityRate::Half),
        ];
        assert_eq!(affinity_response(&odd, 0xFFFF), None);
        assert_eq!(Elemental::of(Some(AffinityRate::Double)), Elemental::Double);
        assert_eq!(Elemental::of(Some(AffinityRate::Half)), Elemental::Half);
        for rate in [
            AffinityRate::Nullify,
            AffinityRate::Absorb,
            AffinityRate::Death,
            AffinityRate::FullHeal,
            AffinityRate::Other(1),
        ] {
            assert_eq!(Elemental::of(Some(rate)), Elemental::Normal);
        }
        assert_eq!(Elemental::of(None), Elemental::Normal);
    }

    /// The hand-made constructors (for rule sets without game files) describe exactly what the decoder
    /// makes of the real kind of record, so a synthetic battle plays like a real one.
    #[test]
    fn the_hand_made_specs_equal_what_the_decoder_makes_of_the_real_kinds_of_record() {
        let from = |calc, power, accuracy, targets, element, flags, mp| {
            ActionSpec::from_attack(&attack_record(
                calc, power, accuracy, targets, element, flags, mp,
            ))
            .unwrap()
        };
        assert_eq!(
            ActionSpec::damage_spell(8, 100, element::FIRE, 4),
            from(0x22, 8, 100, 0x0F, element::FIRE, 0xFDFF, 4)
        );
        assert_eq!(
            ActionSpec::cure_spell(5, 5),
            from(0x25, 5, 255, 0x0D, element::RESTORATIVE, 0xFDFF, 5)
        );
        assert_eq!(
            ActionSpec::revive_spell(8, 34),
            from(0x24, 8, 255, 0x0D, element::RESTORATIVE, 0xFCFD, 34)
        );
        let table = ItemTable::from_kernel(&Kernel::parse(&synthetic_kernel_bytes()).unwrap());
        assert_eq!(
            ActionSpec::flat_restore_item(5, false),
            table.get(0).unwrap().spec,
            "Potion"
        );
        assert_eq!(
            ActionSpec::flat_restore_item(5, true),
            table.get(3).unwrap().spec,
            "Ether"
        );
        assert_eq!(
            ActionSpec::damage_item(10, element::SHOOT),
            table.get(20).unwrap().spec,
            "Grenade"
        );
        assert_eq!(ActionSpec::full_restore_item(), table.get(5).unwrap().spec, "Elixir");
        // Megalixir is Elixir aimed at the whole party (its record also clears the hits-MP bit, which the
        // "everything" formula does not look at).
        assert_eq!(
            ActionSpec { hits_mp: true, ..ActionSpec::full_restore_item().with_aim(Aim::Side) },
            table.get(6).unwrap().spec,
            "Megalixir"
        );
        let hand = ItemTable::new([(0, ItemData::new(ActionSpec::flat_restore_item(5, false)))]);
        assert_eq!(hand.info(0).map(|i| i.kind), Some(ItemKind::RestoreHp));
        assert_eq!(
            Spell::new(27, ActionSpec::damage_spell(8, 100, element::FIRE, 4))
                .info()
                .mp_cost,
            4
        );
    }

    /// Terence Fergusson's rule: the physical formula and Cure's split when they hit several targets, the
    /// spell formula only when the spell can be toggled between one target and all of them ("NS", a forced
    /// whole side, does not), and no other formula ever does.
    #[test]
    fn only_the_formulas_that_split_in_ff7_split_and_a_forced_whole_side_does_not() {
        let spec = |formula, aim| ActionSpec { formula, aim, ..ActionSpec::damage_spell(8, 100, 0, 0) };
        for aim in [Aim::One, Aim::OneOrSide, Aim::Side] {
            assert!(spec(Formula::Standard, aim).splits(), "physical, {aim:?}");
            assert!(spec(Formula::Restore, aim).splits(), "Cure, {aim:?}");
            for never in [Formula::CurrentHp, Formula::MaxHp, Formula::Flat, Formula::Fixed, Formula::Full] {
                assert!(!spec(never, aim).splits(), "{never:?}, {aim:?}");
            }
        }
        assert!(spec(Formula::Spell, Aim::OneOrSide).splits(), "Fire on all");
        assert!(!spec(Formula::Spell, Aim::Side).splits(), "Ultima is No Split");
        assert!(!spec(Formula::Spell, Aim::One).splits());
        // The kinds of record the tests name: Cure and Fire split, Ultima, Healing Wind and Megalixir do not.
        assert!(ActionSpec::cure_spell(5, 5).splits() && ActionSpec::damage_spell(8, 100, element::FIRE, 4).splits());
        assert!(!ActionSpec::limit_heal(16).splits());
        assert!(!ActionSpec::full_restore_item().with_aim(Aim::Side).splits());
        assert!(!ActionSpec::damage_spell(105, 100, 0, 130).with_aim(Aim::Side).splits());
        // What may be aimed how.
        assert!(Aim::One.allows_one() && !Aim::One.allows_side());
        assert!(Aim::OneOrSide.allows_one() && Aim::OneOrSide.allows_side());
        assert!(!Aim::Side.allows_one() && Aim::Side.allows_side());
        assert!(!Aim::Random.allows_one() && !Aim::Random.allows_side());
    }

    #[test]
    fn the_item_table_keeps_what_battle_can_use_and_says_what_each_does() {
        let kernel = Kernel::parse(&synthetic_kernel_bytes()).unwrap();
        let table = ItemTable::from_kernel(&kernel);
        // Potion, Hi-Potion, X-Potion, Ether, Turbo Ether, Elixir, Megalixir (every ally at once), Phoenix
        // Down, Grenade. Not: Antidote (a status), the item that forbids battle use, unused records.
        assert_eq!(
            table.indexes().collect::<Vec<_>>(),
            [0, 1, 2, 3, 4, 5, 6, 7, 20]
        );
        assert_eq!(table.len(), 9);
        assert!(!table.is_empty() && ItemTable::default().is_empty());
        let info = |i| table.info(i).unwrap();
        assert_eq!(
            info(0),
            ItemInfo {
                item: 0,
                kind: ItemKind::RestoreHp,
                targets_allies: true,
                revives: false,
                aim: Aim::One
            }
        );
        assert_eq!(info(2).kind, ItemKind::RestoreHp);
        assert_eq!(info(3).kind, ItemKind::RestoreMp);
        assert_eq!(info(4).kind, ItemKind::RestoreMp);
        assert_eq!(info(5).kind, ItemKind::RestoreAll);
        // Megalixir is Elixir for the whole party: restores everything, aimed at a whole side only.
        assert_eq!(
            info(6),
            ItemInfo {
                item: 6,
                kind: ItemKind::RestoreAll,
                targets_allies: true,
                revives: false,
                aim: Aim::Side
            }
        );
        assert_eq!(info(5).aim, Aim::One);
        assert_eq!(
            info(7),
            ItemInfo {
                item: 7,
                kind: ItemKind::Revive,
                targets_allies: true,
                revives: true,
                aim: Aim::One
            }
        );
        assert_eq!(
            info(20),
            ItemInfo {
                item: 20,
                kind: ItemKind::Damage,
                targets_allies: false,
                revives: false,
                aim: Aim::One
            }
        );
        for gone in [8u16, 100, 127, 128, 9999] {
            assert!(table.info(gone).is_none(), "item {gone}");
        }
        // Potion: a flat heal that cannot miss; Ether hits MP; Phoenix Down hits the dead only.
        let potion = table.get(0).unwrap().spec;
        assert_eq!(
            (potion.formula, potion.power, potion.rolls_accuracy),
            (Formula::Flat, 5, false)
        );
        assert!(table.get(3).unwrap().spec.hits_mp);
        assert!(table.get(7).unwrap().spec.hits_dead);
        // A hand-made table holds exactly what it is given.
        let one = ItemTable::new([(9, *table.get(0).unwrap())]);
        assert_eq!(one.indexes().collect::<Vec<_>>(), [9]);
    }

    // --- statuses (checkpoint 2 of "deeper battles") ---

    /// `rec` with a status: the chance/mode byte and the mask of its record.
    fn with_status(rec: AttackRecord, chance_mode: u8, mask: u32) -> AttackRecord {
        AttackRecord { status_chance_mode: chance_mode, status_mask: mask, ..rec }
    }

    fn effect(statuses: &[Status], chance: u8, mode: StatusMode) -> StatusEffect {
        StatusEffect { set: statuses.iter().copied().collect(), chance, mode }
    }

    #[test]
    fn a_damaging_record_keeps_its_damage_and_gains_the_status_of_its_two_fields() {
        // Bio's kind: damage, then a chance (21 of 63) of Poison.
        let bio = with_status(attack(0x22, 8, 100, 0x0F, element::FIRE, 0xFDFF), 21, Status::Poison.bit());
        let spec = ActionSpec::from_attack(&bio).unwrap();
        assert_eq!((spec.formula, spec.power, spec.kind), (Formula::Spell, 8, ActionKind::Magical));
        assert_eq!(spec.status, Some(effect(&[Status::Poison], 21, StatusMode::Inflict)));
        // The chance is the low six bits, the high bits the mode: 0x40 cures, 0x80 toggles.
        for (byte, expect) in [
            (0x3F, effect(&[Status::Poison], 63, StatusMode::Inflict)),
            (0x40 | 63, effect(&[Status::Poison], 63, StatusMode::Cure)),
            (0x80 | 20, effect(&[Status::Poison], 20, StatusMode::Toggle)),
            (0x00, effect(&[Status::Poison], 0, StatusMode::Inflict)),
        ] {
            let rec = with_status(attack(0x22, 8, 100, 0x0F, 0, 0xFDFF), byte, Status::Poison.bit());
            assert_eq!(ActionSpec::from_attack(&rec).unwrap().status, Some(expect), "{byte:#x}");
        }
        // Several at once, and only the ones the rules play: Poison and Silence stay, Confusion is dropped.
        let mixed = with_status(attack(0x22, 8, 100, 0x0F, 0, 0xFDFF), 30, Status::Poison.bit() | Status::Silence.bit() | Status::Confusion.bit());
        assert_eq!(ActionSpec::from_attack(&mixed).unwrap().status, Some(effect(&[Status::Poison, Status::Silence], 30, StatusMode::Inflict)));
        // Statuses the rules do not play, and the "no status" markers, leave the record exactly as it was.
        let plain = ActionSpec::from_attack(&attack(0x22, 8, 100, 0x0F, 0, 0xFDFF)).unwrap();
        assert_eq!(plain.status, None);
        let unplayed = with_status(attack(0x22, 8, 100, 0x0F, 0, 0xFDFF), 30, Status::Confusion.bit() | Status::Frog.bit() | Status::Death.bit());
        assert_eq!(ActionSpec::from_attack(&unplayed), Some(plain));
        for (byte, mask) in [(0xFF, Status::Poison.bit()), (30, 0xFFFF_FFFF), (0xFF, 0xFFFF_FFFF), (30, 0)] {
            let marker = with_status(attack(0x22, 8, 100, 0x0F, 0, 0xFDFF), byte, mask);
            assert_eq!(ActionSpec::from_attack(&marker), Some(plain), "{byte:#x} {mask:#x}");
        }
        // The action keeps what it was in every other respect.
        assert!(!spec.weapon_attack && !spec.on_allies && spec.aim == Aim::OneOrSide);
        assert!(ActionSpec::basic_attack(16, 100, false).weapon_attack, "only the Attack command is a weapon attack");
    }

    #[test]
    fn a_record_with_no_damage_part_is_a_status_only_action_that_knows_its_side() {
        // Sleep's kind: no Power, an accuracy roll, the cursor on the enemies (0x0F).
        let sleep = ActionSpec::from_attack(&with_status(attack(0x22, 0, 90, 0x0F, 0, 0xFFFF), 63, Status::Sleep.bit())).unwrap();
        assert_eq!((sleep.formula, sleep.power, sleep.kind, sleep.rolls_accuracy), (Formula::None, 0, ActionKind::Magical, true));
        assert!(!sleep.on_allies && !sleep.aims_at_allies() && !sleep.heals() && !sleep.splits());
        assert_eq!((sleep.accuracy, sleep.aim, sleep.mp_cost), (90, Aim::OneOrSide, 7));
        assert_eq!(sleep.status, Some(effect(&[Status::Sleep], 63, StatusMode::Inflict)));
        // Haste's kind: calc 0x50 (never misses), the cursor on the party (0x0D: bit 0x02 clear).
        let haste = ActionSpec::from_attack(&with_status(attack(0x50, 0, 255, 0x0D, 0, 0xFFFF), 63, Status::Haste.bit())).unwrap();
        assert!(haste.on_allies && haste.aims_at_allies() && !haste.rolls_accuracy && haste.formula == Formula::None);
        // Calc 0xFF (no damage byte at all) and a physical record (0x10: a roll with the luck check) are the same.
        for calc in [0xFF, 0x47, 0x10, 0x11] {
            let rec = with_status(attack(calc, 0, 100, 0x03, 0, 0xFFFF), 63, Status::Poison.bit());
            let spec = ActionSpec::from_attack(&rec).unwrap_or_else(|| panic!("{calc:#x}"));
            assert_eq!(spec.formula, Formula::None, "{calc:#x}");
            assert!(!spec.on_allies, "target flags 0x03 start on the enemies: {calc:#x}");
        }
        // Without a status there is nothing to do; with only statuses the rules do not play, still nothing.
        for (calc, byte, mask) in [(0xFF, 0xFF, 0xFFFF_FFFF), (0x50, 63, Status::Confusion.bit()), (0x22, 63, 0), (0x50, 0xFF, Status::Haste.bit())] {
            assert!(ActionSpec::from_attack(&with_status(attack(calc, 0, 100, 0x0D, 0, 0xFFFF), byte, mask)).is_none(), "{calc:#x} {byte:#x}");
        }
        // A record with a Power but no formula to give it (the nybble 0) deals nothing: with a status it is
        // status-only, without one it is not an action.
        let no_formula = with_status(attack(0x20, 9, 100, 0x0F, 0, 0xFFFF), 63, Status::Poison.bit());
        assert_eq!(ActionSpec::from_attack(&no_formula).map(|s| (s.formula, s.power)), Some((Formula::None, 0)));
        assert!(ActionSpec::from_attack(&attack(0x20, 9, 100, 0x0F, 0, 0xFFFF)).is_none());
        // A hand-made one decodes to the same thing.
        let made = ActionSpec::status_spell(effect(&[Status::Sleep], 63, StatusMode::Inflict), 90, 7, false);
        assert_eq!((made.formula, made.accuracy, made.aim, made.on_allies), (sleep.formula, sleep.accuracy, sleep.aim, sleep.on_allies));
        assert_eq!(ActionSpec::status_item(effect(&[Status::Poison], 63, StatusMode::Cure), true).aim, Aim::One);
    }

    #[test]
    fn a_status_only_spell_can_be_cast_and_a_status_only_enemy_attack_is_for_the_ai_scripts_alone() {
        let sleep_record = with_status(attack(0x22, 0, 90, 0x0F, 0, 0xFFFF), 63, Status::Sleep.bit());
        let spell = Spell::from_attack(6, &sleep_record).unwrap();
        assert_eq!((spell.info().targets_allies, spell.info().aim, spell.info().mp_cost), (false, Aim::OneOrSide, 7));
        let regen = Spell::from_attack(8, &with_status(attack(0x22, 0, 255, 0x0D, 0, 0xFFFF), 63, Status::Regen.bit())).unwrap();
        assert!(regen.info().targets_allies);
        // A physical record is not a spell, and a spell with an extra effect is not either.
        assert!(Spell::from_attack(9, &with_status(attack(0x11, 0, 100, 0x03, 0, 0xFFFF), 63, Status::Poison.bit())).is_none());
        let extra = AttackRecord { extra_effect: 3, ..sleep_record };
        assert!(Spell::from_attack(6, &extra).is_none());
        // An enemy: the attacks it picks at random are the damaging ones, with their statuses; the status-only
        // ones decode (a script can use them) but are not in that pool.
        let scene_attack = |id, record| SceneAttack { id, name: "x".into(), record };
        let sting = EnemyAction::from_scene(&scene_attack(
            1,
            with_status(attack(0x11, 16, 100, 0x03, element::HIT, 0xFFFF), 30, Status::Poison.bit()),
        ))
        .unwrap();
        assert_eq!(sting.spec.status, Some(effect(&[Status::Poison], 30, StatusMode::Inflict)));
        assert!(EnemyAction::from_scene(&scene_attack(2, sleep_record)).is_none());
        assert!(ActionSpec::from_attack(&sleep_record).is_some());
        // The script's own pool: an attack with only a status (a curse on the other side, a buff on its own), and nothing else.
        let curse = EnemyAction::status_from_scene(&scene_attack(2, sleep_record)).expect("a curse is a status-only action");
        assert_eq!((curse.id, curse.spec.formula, curse.spec.status.map(|s| s.set)), (2, Formula::None, Some(effect(&[Status::Sleep], 63, StatusMode::Inflict).set)));
        assert!(!curse.spec.on_allies, "the record aims at the other side");
        let buff = EnemyAction::status_from_scene(&scene_attack(3, with_status(attack(0x22, 0, 255, 0x0D, 0, 0xFFFF), 63, Status::Barrier.bit()))).expect("a buff");
        assert!(buff.spec.on_allies && buff.spec.aims_at_allies());
        assert!(EnemyAction::status_from_scene(&scene_attack(1, with_status(attack(0x11, 16, 100, 0x03, element::HIT, 0xFFFF), 30, Status::Poison.bit()))).is_none(), "a damaging attack is not one");
        // No damage, no healing and no status the rules play: an animation, which is no action in either pool.
        let animation = attack(0xFF, 0, 255, 0x03, 0, 0xFFFF);
        assert!(EnemyAction::from_scene(&scene_attack(4, animation)).is_none() && EnemyAction::status_from_scene(&scene_attack(4, animation)).is_none());
    }

    #[test]
    fn an_item_with_only_a_status_is_a_status_item_and_says_which_side_it_aims_at() {
        let kernel = Kernel::parse(&synthetic_kernel_bytes()).unwrap();
        let potion = kernel.item(0).unwrap();
        // An Antidote as the data has it: no damage part, the Poison bit, a cure that always works, on one ally.
        let antidote = ItemRecord { target_flags: 0x01, damage_calc: 0x47, power: 0, status_chance_mode: 0x40 | 63, status_mask: Status::Poison.bit(), element_mask: 0, special_flags: 0xFFFF, ..potion };
        let data = ItemData::from_record(&antidote).expect("an item the rules play");
        assert_eq!(data.kind(), ItemKind::Status);
        assert_eq!(data.spec.status, Some(effect(&[Status::Poison], 63, StatusMode::Cure)));
        assert_eq!(data.info(8), ItemInfo { item: 8, kind: ItemKind::Status, targets_allies: true, revives: false, aim: Aim::One });
        assert!(!data.spec.rolls_accuracy && data.spec.on_allies);
        // Whole party at once (Remedy for all): the same, aimed at a side.
        let party = ItemRecord { target_flags: 0x05, ..antidote };
        assert_eq!(ItemData::from_record(&party).unwrap().spec.aim, Aim::Side);
        // A thrown curse starts on the enemies: it is not aimed at the party.
        let dazers = ItemRecord { target_flags: 0x03, status_chance_mode: 63, status_mask: Status::Paralysis.bit(), ..antidote };
        let curse = ItemData::from_record(&dazers).unwrap();
        assert_eq!((curse.kind(), curse.info(9).targets_allies), (ItemKind::Status, false));
        // The marker of "no status" (the synthetic Antidote has one: a mode byte of 0xFF) and the unplayed ones stay out.
        assert!(ItemData::from_record(&ItemRecord { status_chance_mode: 0xFF, ..antidote }).is_none());
        assert!(ItemData::from_record(&ItemRecord { status_mask: Status::Frog.bit(), ..antidote }).is_none());
        // The restriction bit that forbids battle use still does.
        assert!(ItemData::from_record(&ItemRecord { restrictions: 0xFFFF, ..antidote }).is_none());
        // And a table built from a kernel with such a record has it.
        let mut sections = crate::testutil::synthetic_kernel_sections();
        let items = &mut sections[4].1;
        let at = 8 * crate::kernel::ITEM_RECORD_SIZE;
        items[at + 0x0E] = 0x47;
        items[at + 0x0F] = 0;
        items[at + 0x11] = 0x40 | 63;
        let bytes = crate::testutil::pack_kernel(&sections, &[0, 0]);
        let table = ItemTable::from_kernel(&Kernel::parse(&bytes).unwrap());
        assert_eq!(table.indexes().collect::<Vec<_>>(), [0, 1, 2, 3, 4, 5, 6, 7, 8, 20]);
        assert_eq!(table.info(8).unwrap().kind, ItemKind::Status);
    }
}
