//! Status effects (Poison, Sleep, Haste, ...): what they are, the data that inflicts and cures them, the clocks that
//! run them and what each one does to a fight.
//!
//! The state machine that plays them is in [`crate::rules`]; this module is the model it uses and the part that
//! other code (the client's panels, the server's logs, an enemy's AI script) reads: [`Status`], [`StatusSet`],
//! [`StatusEffect`] (what a record of the owner's data does to statuses) and [`StatusState`] (what one combatant
//! has, with the timers). Everything here is integer arithmetic and total on any input, like the rest of the
//! engine, and none of it draws a random number: the chance of a status is rolled by the rules (one draw per
//! target, see `crate::rules`, "RNG draw order").
//!
//! # The statuses
//!
//! FF7 has 32 status bits, in this order (<https://ff7-mods.github.io/ff7-flat-wiki/FF7/Battle/Status_Effects.html>,
//! <https://wiki.ffrtt.ru/index.php?title=FF7%2FBattle%2FStatus_Effects>): Death, Near-death, Sleep, Poison,
//! Sadness, Fury, Confusion, Silence, Haste, Slow, Stop, Frog, Small, Slow-numb, Petrify, Regen, Barrier,
//! MBarrier, Reflect, Dual, Shield, Death Sentence, Manipulate, Berserk, Peerless, Paralysis, Darkness, Dual
//! Drain, Death Force, Resist, "Lucky Girl", Imprisoned ([`Status`] has all 32, bit for bit, because the records
//! of the game use the masks). The rules **model** [`MODELLED`]: Sleep, Poison, Sadness, Fury, Silence, Haste,
//! Slow, Stop, Regen, Barrier, MBarrier, Paralysis and Darkness. Death is the combatant being out of HP and
//! Near-death is a display derived from its HP ([`crate::rules::Combatant::statuses`] says both). Every other
//! bit decodes and is **ignored**: a record that only has such statuses behaves exactly as it did before they
//! were modelled (a damaging one deals its damage, one that does nothing else is not an action), and no panel
//! ever shows a status the rules do not play.
//!
//! # Where they come from: the records
//!
//! An attack record of the kernel (a spell) or of `scene.bin` (an enemy's attack), and an item record, carry two
//! status fields with the same layout (<https://ff7-mods.github.io/ff7-flat-wiki/FF7/Attack_data.html>): a 32-bit
//! mask (offset `0x14`) and a byte (offset `0x11`) whose low six bits are the **chance, out of 63**, and whose
//! high bits say what the mask does: `0x00` inflicts, `0x40` cures what the target has, `0x80` toggles (cures it
//! if the target has it, inflicts it if not). The mode byte `0xFF` (with the mask `0xFFFFFFFF`) is the "no
//! status" marker of a record, which the owner's data uses for every record without one (checked on the Steam
//! copy by `tests/real_data.rs`: counts only). [`StatusEffect::from_record`] decodes them; a chance of 63 is "always"
//! (the rules roll `below(63)` against it, so 63 never fails and 0 never succeeds).
//!
//! An attack with a damage part and a status part (Bio, most of the enemies' stinging attacks) deals its damage and
//! then tries its status on every target that was hit and still stands. One with **no** damage part (Sleep, Haste,
//! Antidote, Echo Screen: the damage-calculation byte `0x50` or `0xFF`, or a Power of 0) is
//! [`crate::action::Formula::None`]: it only has its status. Immunities are the enemy record's own mask
//! (`status_immunities`, at `0xB0`), whose polarity the data settles: a **set** bit means the enemy can be
//! affected and a clear bit means it is immune (209 of the 625 enemies of the Steam copy have every bit set,
//! and no early enemy has a clear one; the statuses nobody can be affected by are clear for a handful of
//! bosses). A party member has no immunities (armor protections are not modelled). A target that is immune to
//! Haste or Slow is immune to both (Fergusson's FAQ, as quoted by the Fandom wiki).
//!
//! # Clocks and durations
//!
//! The documented timers are Terence Fergusson's *FF7 Battle Mechanics* FAQ, section 2 (the FAQ itself could not
//! be fetched here; its section 2 is quoted by the search results and the Fandom wiki pages cited below, which is
//! what this module follows):
//!
//! * every tick the **Global Timer** goes up by the Speed Value (`32768 / (120 + battle_speed * 15 / 8)`: 54, 91
//!   and 273 at Battle Speed 255, 128 and 0), and a **unit of time** is 8192 points of it ([`TIME_UNIT`]): just
//!   over 30 ticks at the fastest speed, 152 at the slowest, which is the figure the FAQ gives and the test below
//!   reproduces;
//! * every combatant has its own **V-Timer**, which goes up by twice the Speed Value ([`crate::rules::v_timer_increase`]),
//!   so a unit of it is [`V_UNIT`] = 16384 points and lasts the same number of ticks; Haste doubles its pace, Slow
//!   halves it, Stop and Death freeze it. Every time-based status except the ones that freeze it runs on it, so
//!   **Haste doubles and Slow halves how fast Poison, Regen, Barrier and the rest run out, and Stop freezes them**
//!   (<https://finalfantasy.fandom.com/wiki/Final_Fantasy_VII_statuses>). The ATB gauge gains the same way
//!   ([`Pace`]).
//!
//! Durations, in those units (<https://finalfantasy.fandom.com/wiki/Final_Fantasy_VII_statuses> and the page of each
//! status on the same wiki):
//!
//! | status | what it does | lasts |
//! |---|---|---|
//! | Poison | `max HP / 32` every 2.5 units, ignoring Defense and Barrier ([`POISON_PERIOD`]) | until cured or the end of the battle |
//! | Regen | `max HP / 32` every unit ([`REGEN_PERIOD`]) | 32 units ([`REGEN_TICKS`]), so 100% of the HP in all |
//! | Haste, Slow | the pace above | the battle (a Haste cures Slow and the other way round) |
//! | Stop | no turns, nothing runs, the gauge is frozen | 15 units, on the Global Timer |
//! | Paralysis | the same, and no limit gauge is gained | 8 units, on the Global Timer |
//! | Sleep | no turns (the ATB gauge keeps filling); a physical hit wakes it, magic does not | see below |
//! | Silence | no Magic command | until cured |
//! | Darkness | the Attack command's hit rate is halved (party members only: see below) | until cured |
//! | Sadness | damage taken x0.7, the limit gauge fills at half speed | until cured |
//! | Fury | hit rate x0.7 (physical and magical), the limit gauge fills at double speed | until cured |
//! | Barrier | physical damage taken is halved | 30 units |
//! | MBarrier | magical damage taken is halved | 30 units |
//!
//! Stop and Paralysis cannot be timed by the V-Timer they freeze, so they count on the Global Timer (the sources
//! give lengths in "units of time" and do not say which timer counts them: APPROXIMATION). Near-death is "HP at
//! or below 25%" (<https://ff7.info/status-effects.htm>), a display. A knockout removes every status. A status that
//! the target already has is not inflicted again (a timed one has its timer refilled, silently).
//!
//! # APPROXIMATIONS and what is not modelled
//!
//! * **Sleep's length**: the sources say only that Sleep outlasts Stop (15 units) and Paralysis (8); 30 units on the
//!   V-Timer is this crate's figure.
//! * **Darkness** halves the hit rate of the Attack command of a **party member** only: in the game the status
//!   only touches the weapon commands (Attack, Morph, Deathblow, Mug, Slash-All, Flash, 2x-Cut, 4x-Cut) and
//!   enemies do not use them, so a Darkened enemy attacks as before (Fandom, "Final Fantasy VII statuses").
//! * **Fury**'s only documented effects here are the hit rate and the limit gauge; one source (the manual, quoted
//!   on a forum) says it also doubles the damage taken, the Fandom wiki does not, and nothing here does.
//! * **Silence** stops the party's Magic command; an enemy's attack is not stopped by it.
//! * **MBarrier** halves magical damage but not magical healing (the wiki says it halves both; items are
//!   "magical" in the damage-calculation byte, so halving healing would halve potions too).
//! * Sadness and Fury are not carried from one battle to the next (the game keeps them), and there are no armor
//!   protections, no weapon status attacks, no Resist, no Reflect, no Death Sentence, Slow-numb, Petrify,
//!   Confusion, Berserk, Manipulate, Frog, Small or the rest: they decode and are ignored.
//! * A status that is not inflicted (the roll failed, the target is immune) says nothing: there is no event.

use serde::{Deserialize, Serialize};

/// Points of the Global Timer in one unit of time (Fergusson's FAQ, section 2): at the fastest Battle Speed
/// (Speed Value 273) a unit takes 31 ticks ("just over 30"), at the slowest (54) it takes 152.
pub const TIME_UNIT: u32 = 8192;
/// Points of a combatant's V-Timer in one unit: the V-Timer runs at twice the Global Timer's pace, so a unit of
/// it takes as many ticks as a Global unit.
pub const V_UNIT: u32 = 2 * TIME_UNIT;
/// Poison hurts every 2.5 units of the V-Timer (Fandom, "Poison (Final Fantasy VII status)").
pub const POISON_PERIOD: u32 = V_UNIT * 5 / 2;
/// Poison takes `max HP / POISON_DIVISOR` each time.
pub const POISON_DIVISOR: u32 = 32;
/// Regen heals every unit of the V-Timer (Fandom, "Regen (Final Fantasy VII status)").
pub const REGEN_PERIOD: u32 = V_UNIT;
/// Regen heals `max HP / REGEN_DIVISOR` each time.
pub const REGEN_DIVISOR: u32 = 32;
/// Regen heals this many times and then ends (32 units).
pub const REGEN_TICKS: u8 = 32;
/// Stop lasts 15 units of the Global Timer (Fandom, "Stop (Final Fantasy VII status)").
pub const STOP_DURATION: u32 = 15 * TIME_UNIT;
/// Paralysis lasts 8 units of the Global Timer (Fandom, "Paralyzed (Final Fantasy VII)").
pub const PARALYSIS_DURATION: u32 = 8 * TIME_UNIT;
/// Sleep lasts 30 V-Timer units (APPROXIMATION, see the module docs).
pub const SLEEP_DURATION: u32 = 30 * V_UNIT;
/// Barrier and MBarrier last 30 V-Timer units (Fandom, "Barrier (Final Fantasy VII status)").
pub const BARRIER_DURATION: u32 = 30 * V_UNIT;
/// The "out of" of a status chance: a record's chance byte is `0..=63`.
pub const CHANCE_OUT_OF: u32 = 63;

/// One of FF7's 32 statuses, numbered as the bits of the status masks of attack, item and enemy records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Status {
    /// Out of HP. Not stored: the combatant's HP is the truth ([`crate::rules::Combatant::statuses`]).
    Death,
    /// HP at or below a quarter of the maximum, and alive. A display, derived from HP.
    NearDeath,
    Sleep,
    Poison,
    Sadness,
    Fury,
    Confusion,
    Silence,
    Haste,
    Slow,
    Stop,
    Frog,
    Small,
    SlowNumb,
    Petrify,
    Regen,
    Barrier,
    MBarrier,
    Reflect,
    Dual,
    Shield,
    DeathSentence,
    Manipulate,
    Berserk,
    Peerless,
    Paralysis,
    Darkness,
    DualDrain,
    DeathForce,
    Resist,
    LuckyGirl,
    Imprisoned,
}

impl Status {
    /// All 32, in bit order.
    pub const ALL: [Status; 32] = [
        Status::Death,
        Status::NearDeath,
        Status::Sleep,
        Status::Poison,
        Status::Sadness,
        Status::Fury,
        Status::Confusion,
        Status::Silence,
        Status::Haste,
        Status::Slow,
        Status::Stop,
        Status::Frog,
        Status::Small,
        Status::SlowNumb,
        Status::Petrify,
        Status::Regen,
        Status::Barrier,
        Status::MBarrier,
        Status::Reflect,
        Status::Dual,
        Status::Shield,
        Status::DeathSentence,
        Status::Manipulate,
        Status::Berserk,
        Status::Peerless,
        Status::Paralysis,
        Status::Darkness,
        Status::DualDrain,
        Status::DeathForce,
        Status::Resist,
        Status::LuckyGirl,
        Status::Imprisoned,
    ];

    /// The bit number (`0..=31`) in a status mask.
    pub fn index(self) -> u8 {
        self as u8
    }

    /// The status of bit number `index`, or `None` past 31.
    pub fn from_index(index: u8) -> Option<Status> {
        Status::ALL.get(usize::from(index)).copied()
    }

    /// The single bit of the status in a mask.
    pub fn bit(self) -> u32 {
        1 << self.index()
    }

    /// The status's English name, as the game's menus write it.
    pub fn name(self) -> &'static str {
        match self {
            Status::Death => "Death",
            Status::NearDeath => "Near-death",
            Status::Sleep => "Sleep",
            Status::Poison => "Poison",
            Status::Sadness => "Sadness",
            Status::Fury => "Fury",
            Status::Confusion => "Confusion",
            Status::Silence => "Silence",
            Status::Haste => "Haste",
            Status::Slow => "Slow",
            Status::Stop => "Stop",
            Status::Frog => "Frog",
            Status::Small => "Small",
            Status::SlowNumb => "Slow-numb",
            Status::Petrify => "Petrify",
            Status::Regen => "Regen",
            Status::Barrier => "Barrier",
            Status::MBarrier => "MBarrier",
            Status::Reflect => "Reflect",
            Status::Dual => "Dual",
            Status::Shield => "Shield",
            Status::DeathSentence => "Death Sentence",
            Status::Manipulate => "Manipulate",
            Status::Berserk => "Berserk",
            Status::Peerless => "Peerless",
            Status::Paralysis => "Paralysis",
            Status::Darkness => "Darkness",
            Status::DualDrain => "Dual Drain",
            Status::DeathForce => "Death Force",
            Status::Resist => "Resist",
            Status::LuckyGirl => "Lucky Girl",
            Status::Imprisoned => "Imprisoned",
        }
    }

    /// The status called `name` (its [`Status::name`], or its variant name, in any case, with or without spaces and
    /// hyphens), or `None`.
    pub fn from_name(name: &str) -> Option<Status> {
        let key: String = name.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_lowercase()).collect();
        Status::ALL.iter().copied().find(|s| {
            let own: String = s.name().chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_lowercase()).collect();
            own == key
        })
    }

    /// Whether the rules play this status (see the module docs): it can be stored, inflicted and cured.
    pub fn is_modelled(self) -> bool {
        MODELLED.contains(self)
    }

    /// Whether the status helps the combatant that has it (a buff) rather than hurting it.
    pub fn is_beneficial(self) -> bool {
        matches!(
            self,
            Status::Haste
                | Status::Regen
                | Status::Barrier
                | Status::MBarrier
                | Status::Reflect
                | Status::Dual
                | Status::Shield
                | Status::Peerless
                | Status::Resist
                | Status::DeathForce
        )
    }

    /// The status that cannot be had together with this one: inflicting one removes the other (Haste and
    /// Slow cancel each other; so do Sadness and Fury, as the Tranquilizer and the Hyper show).
    pub fn opposite(self) -> Option<Status> {
        match self {
            Status::Haste => Some(Status::Slow),
            Status::Slow => Some(Status::Haste),
            Status::Sadness => Some(Status::Fury),
            Status::Fury => Some(Status::Sadness),
            _ => None,
        }
    }
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// A set of statuses: the 32-bit mask of the game's records, one [`Status`] per bit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StatusSet(u32);

/// The statuses the rules play (see the module docs).
pub const MODELLED: StatusSet = StatusSet(
    (1 << 2)      // Sleep
        | (1 << 3)  // Poison
        | (1 << 4)  // Sadness
        | (1 << 5)  // Fury
        | (1 << 7)  // Silence
        | (1 << 8)  // Haste
        | (1 << 9)  // Slow
        | (1 << 10) // Stop
        | (1 << 15) // Regen
        | (1 << 16) // Barrier
        | (1 << 17) // MBarrier
        | (1 << 25) // Paralysis
        | (1 << 26), // Darkness
);

impl StatusSet {
    /// No status.
    pub const EMPTY: StatusSet = StatusSet(0);

    /// The set a mask of a record stands for (a bit of it per [`Status`]).
    pub const fn from_bits(bits: u32) -> StatusSet {
        StatusSet(bits)
    }

    /// The mask.
    pub const fn bits(self) -> u32 {
        self.0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn len(self) -> usize {
        self.0.count_ones() as usize
    }

    pub fn contains(self, status: Status) -> bool {
        self.0 & status.bit() != 0
    }

    /// Whether the two sets have a status in common.
    pub fn intersects(self, other: StatusSet) -> bool {
        self.0 & other.0 != 0
    }

    pub fn insert(&mut self, status: Status) {
        self.0 |= status.bit();
    }

    pub fn remove(&mut self, status: Status) {
        self.0 &= !status.bit();
    }

    /// The set with `status` added.
    pub fn with(mut self, status: Status) -> StatusSet {
        self.insert(status);
        self
    }

    /// The set with `status` taken out.
    pub fn without(mut self, status: Status) -> StatusSet {
        self.remove(status);
        self
    }

    pub fn union(self, other: StatusSet) -> StatusSet {
        StatusSet(self.0 | other.0)
    }

    pub fn intersection(self, other: StatusSet) -> StatusSet {
        StatusSet(self.0 & other.0)
    }

    /// The statuses of `self` that `other` does not have.
    pub fn difference(self, other: StatusSet) -> StatusSet {
        StatusSet(self.0 & !other.0)
    }

    /// The statuses in the set, in bit order.
    pub fn iter(self) -> impl Iterator<Item = Status> {
        Status::ALL.into_iter().filter(move |s| self.contains(*s))
    }
}

impl FromIterator<Status> for StatusSet {
    fn from_iter<I: IntoIterator<Item = Status>>(iter: I) -> Self {
        iter.into_iter().fold(StatusSet::EMPTY, StatusSet::with)
    }
}

impl std::fmt::Display for StatusSet {
    /// The names, comma separated (`Poison, Haste`), or `-` for none.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_empty() {
            return f.write_str("-");
        }
        let names: Vec<&str> = self.iter().map(Status::name).collect();
        f.write_str(&names.join(", "))
    }
}

/// What an action does to statuses: decoded from the status mask and the chance/mode byte of its record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusEffect {
    /// The modelled statuses of the record's mask (never empty; the bits the rules do not play are dropped).
    pub set: StatusSet,
    /// The chance out of [`CHANCE_OUT_OF`] (63 is "always").
    pub chance: u8,
    pub mode: StatusMode,
}

/// What a record's status mask does to the statuses a target has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusMode {
    /// Inflicts the statuses it does not have.
    Inflict,
    /// Cures the statuses it has.
    Cure,
    /// Cures those it has and inflicts those it has not.
    Toggle,
}

impl StatusEffect {
    /// The effect of a record whose status mask is `mask` and whose chance/mode byte is `chance_mode`, or `None`
    /// when it has none: the "no status" marker (mode byte `0xFF` or mask `0xFFFFFFFF`), an empty mask, or a mask of
    /// statuses the rules do not model. The chance is the low six bits (`0..=63`); bit `0x80` toggles, else bit
    /// `0x40` cures, else the mask is inflicted (see the module docs).
    pub fn from_record(mask: u32, chance_mode: u8) -> Option<StatusEffect> {
        if chance_mode == 0xFF || mask == 0xFFFF_FFFF {
            return None;
        }
        let set = StatusSet::from_bits(mask).intersection(MODELLED);
        if set.is_empty() {
            return None;
        }
        let mode = if chance_mode & 0x80 != 0 {
            StatusMode::Toggle
        } else if chance_mode & 0x40 != 0 {
            StatusMode::Cure
        } else {
            StatusMode::Inflict
        };
        Some(StatusEffect { set, chance: chance_mode & 0x3F, mode })
    }

    /// An effect that inflicts `status` with the given chance (out of 63), for hand-made data.
    pub fn inflict(status: Status, chance: u8) -> StatusEffect {
        StatusEffect { set: StatusSet::EMPTY.with(status), chance: chance.min(CHANCE_OUT_OF as u8), mode: StatusMode::Inflict }
    }

    /// An effect that cures `status` with the given chance (out of 63), for hand-made data.
    pub fn cure(status: Status, chance: u8) -> StatusEffect {
        StatusEffect { mode: StatusMode::Cure, ..StatusEffect::inflict(status, chance) }
    }

    /// Whether the effect helps its target: a cure, or an inflicted buff. (A toggle of ailments helps one that
    /// has them and hurts one that has not; it counts as hurting.)
    pub fn helps(&self) -> bool {
        match self.mode {
            StatusMode::Cure => true,
            StatusMode::Inflict => self.set.iter().all(Status::is_beneficial),
            StatusMode::Toggle => false,
        }
    }

    /// The chance as the rules use it: out of 63, cut to 2/3 when the action splits its effect between several
    /// targets (Fergusson's FAQ: the chance of a status that comes with a split attack is cut to 2/3 too).
    pub fn chance_among(&self, split: bool) -> u32 {
        let chance = u32::from(self.chance).min(CHANCE_OUT_OF);
        if split {
            chance * 2 / 3
        } else {
            chance
        }
    }
}

/// How fast a combatant's own clock (and ATB gauge) runs, from its statuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pace {
    /// Stop or Paralysis: nothing moves.
    Frozen,
    /// Slow: half.
    Slow,
    Normal,
    /// Haste: double.
    Haste,
}

impl Pace {
    /// `points` of a clock or a gauge, at this pace.
    pub fn scale(self, points: u32) -> u32 {
        match self {
            Pace::Frozen => 0,
            Pace::Slow => points / 2,
            Pace::Normal => points,
            Pace::Haste => points.saturating_mul(2),
        }
    }
}

/// One change to a combatant's statuses, as an action or a clock makes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// The combatant now has this status.
    Inflicted(Status),
    /// The combatant no longer has this status (cured, worn off, or removed by a blow).
    Ended(Status),
}

/// What one tick of a combatant's clock did (see [`StatusState::advance`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Elapsed {
    /// Poison is due: it takes `max HP / 32`.
    pub poison: bool,
    /// Regen is due: it gives `max HP / 32`.
    pub regen: bool,
    /// The timed statuses that ran out on this tick.
    pub ended: StatusSet,
}

/// The statuses one combatant has, and the timers of the timed ones. Battle state: it is never sent or kept (the
/// events say what changed, and the starting statuses are an input of the combatant).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StatusState {
    set: StatusSet,
    /// Points left (V-Timer) before Sleep ends.
    sleep_left: u32,
    /// Points left (Global Timer) before Stop ends.
    stop_left: u32,
    /// Points left (Global Timer) before Paralysis ends.
    paralysis_left: u32,
    /// Points left (V-Timer) before Barrier ends.
    barrier_left: u32,
    /// Points left (V-Timer) before MBarrier ends.
    mbarrier_left: u32,
    /// V-Timer points since Poison last hurt.
    poison_acc: u32,
    /// V-Timer points since Regen last healed.
    regen_acc: u32,
    /// How many more times Regen heals.
    regen_left: u8,
}

impl StatusState {
    /// No status.
    pub fn none() -> Self {
        Self::default()
    }

    /// The state of a combatant that starts a battle with `set` (the modelled statuses of it; the timed ones start
    /// with their whole duration).
    pub fn starting(set: StatusSet) -> Self {
        let mut state = Self::default();
        for status in set.intersection(MODELLED).iter() {
            state.add(status);
        }
        state
    }

    /// The stored statuses (never Death or Near-death: see [`crate::rules::Combatant::statuses`]).
    pub fn set(&self) -> StatusSet {
        self.set
    }

    pub fn has(&self, status: Status) -> bool {
        self.set.contains(status)
    }

    pub fn is_empty(&self) -> bool {
        self.set.is_empty()
    }

    /// Sleep, Stop or Paralysis: the combatant takes no turn.
    pub fn blocks_turns(&self) -> bool {
        self.has(Status::Sleep) || self.has(Status::Stop) || self.has(Status::Paralysis)
    }

    /// Silence: the Magic command is closed.
    pub fn silenced(&self) -> bool {
        self.has(Status::Silence)
    }

    /// How fast the combatant's clock and gauge run: Stop and Paralysis freeze them, Haste doubles them, Slow halves
    /// them.
    pub fn pace(&self) -> Pace {
        if self.has(Status::Stop) || self.has(Status::Paralysis) {
            Pace::Frozen
        } else if self.has(Status::Haste) {
            Pace::Haste
        } else if self.has(Status::Slow) {
            Pace::Slow
        } else {
            Pace::Normal
        }
    }

    /// How long a timed status has left, in points of the clock that counts it (the Global Timer for Stop and
    /// Paralysis, the V-Timer for the others), or `None` for a status without a timer or one the combatant has not
    /// got. Regen counts its heals instead: see [`StatusState::regen_left`].
    pub fn points_left(&self, status: Status) -> Option<u32> {
        if !self.has(status) {
            return None;
        }
        match status {
            Status::Sleep => Some(self.sleep_left),
            Status::Stop => Some(self.stop_left),
            Status::Paralysis => Some(self.paralysis_left),
            Status::Barrier => Some(self.barrier_left),
            Status::MBarrier => Some(self.mbarrier_left),
            _ => None,
        }
    }

    /// How many more times Regen heals (0 without Regen).
    pub fn regen_left(&self) -> u8 {
        if self.has(Status::Regen) {
            self.regen_left
        } else {
            0
        }
    }

    /// Give the combatant `status` with its whole timer (no change to what else it has).
    fn add(&mut self, status: Status) {
        self.set.insert(status);
        self.restart(status);
    }

    /// Refill the timer of a timed status the combatant has (a new one, or one inflicted again).
    fn restart(&mut self, status: Status) {
        match status {
            Status::Sleep => self.sleep_left = SLEEP_DURATION,
            Status::Stop => self.stop_left = STOP_DURATION,
            Status::Paralysis => self.paralysis_left = PARALYSIS_DURATION,
            Status::Barrier => self.barrier_left = BARRIER_DURATION,
            Status::MBarrier => self.mbarrier_left = BARRIER_DURATION,
            Status::Regen => {
                self.regen_left = REGEN_TICKS;
                self.regen_acc = 0;
            }
            _ => {}
        }
    }

    fn remove(&mut self, status: Status) {
        self.set.remove(status);
        match status {
            Status::Sleep => self.sleep_left = 0,
            Status::Stop => self.stop_left = 0,
            Status::Paralysis => self.paralysis_left = 0,
            Status::Barrier => self.barrier_left = 0,
            Status::MBarrier => self.mbarrier_left = 0,
            Status::Poison => self.poison_acc = 0,
            Status::Regen => {
                self.regen_left = 0;
                self.regen_acc = 0;
            }
            _ => {}
        }
    }

    /// Wake a sleeper (a physical blow): `true` when it was asleep.
    pub(crate) fn wake(&mut self) -> bool {
        let asleep = self.has(Status::Sleep);
        if asleep {
            self.remove(Status::Sleep);
        }
        asleep
    }

    /// Take every status away (a knockout).
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }

    /// What `effect` does to this combatant once its chance has been rolled and succeeded: the statuses it
    /// inflicts (the ones this combatant does not have and is not `immune` to; Haste and Slow share their immunity,
    /// and each cancels the other, like Sadness and Fury), cures, or toggles, in bit order. A status it already has
    /// is not inflicted again (a timed one is refilled, silently). Bits the rules do not model are ignored.
    pub(crate) fn apply(&mut self, effect: &StatusEffect, immune: StatusSet) -> Vec<Change> {
        let mut changes = Vec::new();
        for status in effect.set.intersection(MODELLED).iter() {
            let has = self.has(status);
            let inflict = match effect.mode {
                StatusMode::Inflict => true,
                StatusMode::Cure => false,
                StatusMode::Toggle => !has,
            };
            if !inflict {
                if has {
                    self.remove(status);
                    changes.push(Change::Ended(status));
                }
                continue;
            }
            if has {
                self.restart(status);
                continue;
            }
            let linked_immunity = matches!(status, Status::Haste | Status::Slow) && (immune.contains(Status::Haste) || immune.contains(Status::Slow));
            if immune.contains(status) || linked_immunity {
                continue;
            }
            if let Some(other) = status.opposite() {
                if self.has(other) {
                    self.remove(other);
                    changes.push(Change::Ended(other));
                }
            }
            self.add(status);
            changes.push(Change::Inflicted(status));
        }
        changes
    }

    /// One tick of the combatant's clock. `v_timer_increase` is the battle's V-Timer increase per tick
    /// ([`crate::rules::v_timer_increase`]): Stop and Paralysis count on the Global Timer (half of it), everything
    /// else on this combatant's V-Timer at its [`Pace`]. Returns what is due: Poison's and Regen's turns to act
    /// and the statuses that ran out (already removed).
    pub(crate) fn advance(&mut self, v_timer_increase: u32) -> Elapsed {
        let global = v_timer_increase / 2;
        let v = self.pace().scale(v_timer_increase);
        let mut out = Elapsed::default();
        for (status, left, points) in [(Status::Stop, self.stop_left, global), (Status::Paralysis, self.paralysis_left, global)] {
            if self.has(status) && self.run_down(status, left, points) {
                out.ended.insert(status);
            }
        }
        if v == 0 {
            return out;
        }
        for (status, left) in [
            (Status::Sleep, self.sleep_left),
            (Status::Barrier, self.barrier_left),
            (Status::MBarrier, self.mbarrier_left),
        ] {
            if self.has(status) && self.run_down(status, left, v) {
                out.ended.insert(status);
            }
        }
        if self.has(Status::Poison) {
            self.poison_acc += v;
            if self.poison_acc >= POISON_PERIOD {
                self.poison_acc -= POISON_PERIOD;
                out.poison = true;
            }
        }
        if self.has(Status::Regen) {
            self.regen_acc += v;
            if self.regen_acc >= REGEN_PERIOD {
                self.regen_acc -= REGEN_PERIOD;
                out.regen = true;
                self.regen_left = self.regen_left.saturating_sub(1);
                if self.regen_left == 0 {
                    self.remove(Status::Regen);
                    out.ended.insert(Status::Regen);
                }
            }
        }
        out
    }

    /// Take `points` off the timer of `status` (which had `left`); remove it when it has run out. `true` when it did.
    fn run_down(&mut self, status: Status, left: u32, points: u32) -> bool {
        let left = left.saturating_sub(points);
        if left == 0 {
            self.remove(status);
            return true;
        }
        match status {
            Status::Sleep => self.sleep_left = left,
            Status::Stop => self.stop_left = left,
            Status::Paralysis => self.paralysis_left = left,
            Status::Barrier => self.barrier_left = left,
            Status::MBarrier => self.mbarrier_left = left,
            _ => {}
        }
        false
    }
}

/// The damage a blow does after the statuses of the one it hits (applied to the amount the damage formulas gave):
/// Barrier halves a physical blow, MBarrier a magical one, Sadness takes 30% off either (never below 1).
pub fn damage_after_statuses(amount: u32, physical: bool, target: &StatusState) -> u32 {
    let mut amount = amount;
    if (physical && target.has(Status::Barrier)) || (!physical && target.has(Status::MBarrier)) {
        amount /= 2;
    }
    if target.has(Status::Sadness) {
        amount = amount * 7 / 10;
    }
    amount.max(1)
}

/// A hit chance (a percentage, possibly above 100) after the statuses of the one who strikes: Fury takes 30% off
/// it (physical and magical), and Darkness halves it when the blow is the Attack command of a party member
/// (`weapon_attack`; see the module docs for why not an enemy's).
pub fn hit_chance_after_statuses(chance: i64, user: &StatusState, weapon_attack: bool, party_member: bool) -> i64 {
    let mut chance = chance;
    if weapon_attack && party_member && user.has(Status::Darkness) {
        chance /= 2;
    }
    if user.has(Status::Fury) {
        chance = chance * 7 / 10;
    }
    chance
}

/// The limit gauge gained from a blow after the statuses of the one hit: none while Paralysed, half under Sadness,
/// double under Fury.
pub fn limit_gain_after_statuses(gain: u16, target: &StatusState) -> u16 {
    if target.has(Status::Paralysis) {
        0
    } else if target.has(Status::Sadness) {
        gain / 2
    } else if target.has(Status::Fury) {
        gain.saturating_mul(2)
    } else {
        gain
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_32_statuses_are_numbered_like_the_bits_of_the_records() {
        assert_eq!(Status::ALL.len(), 32);
        for (i, status) in Status::ALL.iter().enumerate() {
            assert_eq!(usize::from(status.index()), i);
            assert_eq!(status.bit(), 1u32 << i);
            assert_eq!(Status::from_index(i as u8), Some(*status));
        }
        assert_eq!(Status::from_index(32), None);
        // A few bits the sources name (the ff7-flat-wiki table): Poison is 0x8, Silence 0x80, Haste 0x100, Stop 0x400,
        // Regen 0x8000, Barrier 0x10000, Paralysis 0x02000000, Darkness 0x04000000.
        assert_eq!(
            [Status::Poison.bit(), Status::Silence.bit(), Status::Haste.bit(), Status::Stop.bit(), Status::Regen.bit(), Status::Barrier.bit()],
            [0x8, 0x80, 0x100, 0x400, 0x8000, 0x1_0000]
        );
        assert_eq!([Status::Paralysis.bit(), Status::Darkness.bit(), Status::Imprisoned.bit()], [0x0200_0000, 0x0400_0000, 0x8000_0000]);
    }

    #[test]
    fn names_are_unique_and_found_in_any_spelling() {
        let mut names: Vec<&str> = Status::ALL.iter().map(|s| s.name()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 32);
        for status in Status::ALL {
            assert_eq!(Status::from_name(status.name()), Some(status), "{status}");
            assert_eq!(Status::from_name(&format!("{status:?}").to_uppercase()), Some(status), "{status:?}");
        }
        assert_eq!(Status::from_name("m-barrier"), Some(Status::MBarrier));
        assert_eq!(Status::from_name("near death"), Some(Status::NearDeath));
        assert_eq!(Status::from_name("poisonous"), None);
        assert_eq!(Status::from_name(""), None);
    }

    #[test]
    fn a_set_is_the_mask_of_its_statuses() {
        let set = StatusSet::EMPTY.with(Status::Poison).with(Status::Haste);
        assert_eq!(set.bits(), 0x108);
        assert_eq!((set.len(), set.is_empty()), (2, false));
        assert!(set.contains(Status::Poison) && !set.contains(Status::Slow));
        assert_eq!(set.iter().collect::<Vec<_>>(), [Status::Poison, Status::Haste]);
        assert_eq!(set.without(Status::Poison).bits(), 0x100);
        assert_eq!(set.to_string(), "Poison, Haste");
        assert_eq!(StatusSet::EMPTY.to_string(), "-");
        assert_eq!([Status::Slow, Status::Sleep].into_iter().collect::<StatusSet>().bits(), 0x204);
        let other = StatusSet::from_bits(0x18);
        assert_eq!((set.union(other).bits(), set.intersection(other).bits(), set.difference(other).bits()), (0x118, 0x8, 0x100));
        assert!(set.intersects(other) && !set.intersects(StatusSet::from_bits(0x4)));
    }

    #[test]
    fn a_set_travels_as_its_mask_and_a_status_as_its_name() {
        assert_eq!(serde_json::to_string(&StatusSet::from_bits(0x108)).unwrap(), "264");
        assert_eq!(serde_json::from_str::<StatusSet>("264").unwrap(), StatusSet::from_bits(0x108));
        assert_eq!(serde_json::to_string(&Status::MBarrier).unwrap(), "\"MBarrier\"");
        for status in Status::ALL {
            let json = serde_json::to_string(&status).unwrap();
            assert_eq!(serde_json::from_str::<Status>(&json).unwrap(), status);
        }
        assert!(serde_json::from_str::<Status>("\"Cheerful\"").is_err());
        assert!(serde_json::from_str::<Status>("3").is_err(), "a status is its name, not a bit number");
    }

    #[test]
    fn exactly_the_documented_statuses_are_modelled() {
        let names: Vec<&str> = MODELLED.iter().map(Status::name).collect();
        assert_eq!(
            names,
            ["Sleep", "Poison", "Sadness", "Fury", "Silence", "Haste", "Slow", "Stop", "Regen", "Barrier", "MBarrier", "Paralysis", "Darkness"]
        );
        // Death and Near-death are derived from HP, never stored.
        assert!(!Status::Death.is_modelled() && !Status::NearDeath.is_modelled());
        assert!(Status::Haste.is_beneficial() && Status::Regen.is_beneficial() && !Status::Poison.is_beneficial());
        assert_eq!((Status::Haste.opposite(), Status::Fury.opposite(), Status::Poison.opposite()), (Some(Status::Slow), Some(Status::Sadness), None));
    }

    #[test]
    fn a_record_decodes_to_its_effect_or_to_nothing() {
        // The marker of "no status" and the empty mask.
        assert_eq!(StatusEffect::from_record(0xFFFF_FFFF, 0xFF), None);
        assert_eq!(StatusEffect::from_record(0xFFFF_FFFF, 0x3F), None, "no real mask has every bit");
        assert_eq!(StatusEffect::from_record(0x8, 0xFF), None);
        assert_eq!(StatusEffect::from_record(0, 0x3F), None);
        // Only unmodelled statuses (Confusion, Frog, Petrify): nothing the rules play.
        assert_eq!(StatusEffect::from_record(0x40 | 0x800 | 0x4000, 0x3F), None);
        // Poison, always (the chance byte 0x3F: 63 of 63), and cured (0x7F) by an Antidote.
        let poison = StatusEffect::from_record(0x8, 0x3F).unwrap();
        assert_eq!((poison.set.bits(), poison.chance, poison.mode), (0x8, 63, StatusMode::Inflict));
        assert_eq!(StatusEffect::from_record(0x8, 0x7F).unwrap().mode, StatusMode::Cure);
        // A toggle with a chance of 18 (0x92), and the unmodelled bits of a mixed mask are dropped.
        let toggle = StatusEffect::from_record(0x0000_0804, 0x92).unwrap();
        assert_eq!((toggle.set.bits(), toggle.chance, toggle.mode), (0x4, 18, StatusMode::Toggle));
        // Bit 0x40 with the toggle bit is a toggle, not a cure.
        assert_eq!(StatusEffect::from_record(0x8, 0xC5).unwrap().mode, StatusMode::Toggle);
        // Death in a mask (Life, Phoenix Down) is not a status the rules store.
        assert_eq!(StatusEffect::from_record(0x1, 0x7F), None);
        assert_eq!(StatusEffect::from_record(0x9, 0x7F).unwrap().set.bits(), 0x8);
    }

    #[test]
    fn a_split_action_cuts_the_chance_to_two_thirds_and_helping_is_a_cure_or_a_buff() {
        let always = StatusEffect::inflict(Status::Sleep, 63);
        assert_eq!((always.chance_among(false), always.chance_among(true)), (63, 42));
        assert_eq!(StatusEffect::inflict(Status::Sleep, 200).chance, 63, "a chance is out of 63 at most");
        assert!(StatusEffect::cure(Status::Poison, 63).helps());
        assert!(StatusEffect::inflict(Status::Haste, 63).helps());
        assert!(!StatusEffect::inflict(Status::Poison, 63).helps());
        let toggle = StatusEffect { mode: StatusMode::Toggle, ..always };
        assert!(!toggle.helps());
    }

    #[test]
    fn inflicting_curing_and_toggling_report_what_changed_in_bit_order() {
        let mut s = StatusState::none();
        let both = StatusEffect { set: StatusSet::from_bits(Status::Poison.bit() | Status::Sleep.bit()), chance: 63, mode: StatusMode::Inflict };
        assert_eq!(s.apply(&both, StatusSet::EMPTY), [Change::Inflicted(Status::Sleep), Change::Inflicted(Status::Poison)]);
        // Again: nothing new.
        assert_eq!(s.apply(&both, StatusSet::EMPTY), []);
        let cure = StatusEffect { mode: StatusMode::Cure, ..both };
        assert_eq!(s.apply(&cure, StatusSet::EMPTY), [Change::Ended(Status::Sleep), Change::Ended(Status::Poison)]);
        assert_eq!(s.apply(&cure, StatusSet::EMPTY), [], "nothing to cure");
        // A toggle inflicts what is missing and cures what is there.
        s.apply(&StatusEffect::inflict(Status::Poison, 63), StatusSet::EMPTY);
        let toggle = StatusEffect { mode: StatusMode::Toggle, ..both };
        assert_eq!(s.apply(&toggle, StatusSet::EMPTY), [Change::Inflicted(Status::Sleep), Change::Ended(Status::Poison)]);
        // Bits the rules do not model do nothing.
        let frog = StatusEffect { set: StatusSet::from_bits(Status::Frog.bit()), chance: 63, mode: StatusMode::Inflict };
        assert_eq!(StatusState::none().apply(&frog, StatusSet::EMPTY), []);
    }

    #[test]
    fn immunity_blocks_an_inflict_but_not_a_cure_and_haste_and_slow_share_it() {
        let mut s = StatusState::none();
        let immune = StatusSet::from_bits(Status::Sleep.bit() | Status::Slow.bit());
        assert_eq!(s.apply(&StatusEffect::inflict(Status::Sleep, 63), immune), []);
        assert!(!s.has(Status::Sleep));
        // Immune to Slow means immune to Haste too (and the other way round).
        assert_eq!(s.apply(&StatusEffect::inflict(Status::Haste, 63), immune), []);
        assert_eq!(StatusState::none().apply(&StatusEffect::inflict(Status::Slow, 63), StatusSet::EMPTY.with(Status::Haste)), []);
        // Something else is not blocked.
        assert_eq!(s.apply(&StatusEffect::inflict(Status::Poison, 63), immune), [Change::Inflicted(Status::Poison)]);
        // A cure works whatever the target is immune to.
        let mut asleep = StatusState::starting(StatusSet::EMPTY.with(Status::Sleep));
        assert_eq!(asleep.apply(&StatusEffect::cure(Status::Sleep, 63), immune), [Change::Ended(Status::Sleep)]);
    }

    #[test]
    fn haste_and_slow_cancel_each_other_and_so_do_sadness_and_fury() {
        let mut s = StatusState::none();
        s.apply(&StatusEffect::inflict(Status::Slow, 63), StatusSet::EMPTY);
        assert_eq!(s.pace(), Pace::Slow);
        assert_eq!(s.apply(&StatusEffect::inflict(Status::Haste, 63), StatusSet::EMPTY), [Change::Ended(Status::Slow), Change::Inflicted(Status::Haste)]);
        assert_eq!((s.pace(), s.has(Status::Slow)), (Pace::Haste, false));
        s.apply(&StatusEffect::inflict(Status::Sadness, 63), StatusSet::EMPTY);
        assert_eq!(s.apply(&StatusEffect::inflict(Status::Fury, 63), StatusSet::EMPTY), [Change::Ended(Status::Sadness), Change::Inflicted(Status::Fury)]);
        assert!(s.has(Status::Fury) && !s.has(Status::Sadness));
    }

    #[test]
    fn starting_statuses_keep_only_the_modelled_ones_with_their_whole_timers() {
        let s = StatusState::starting(StatusSet::from_bits(Status::Death.bit() | Status::Confusion.bit() | Status::Stop.bit() | Status::Regen.bit() | Status::Barrier.bit()));
        assert_eq!(s.set().iter().collect::<Vec<_>>(), [Status::Stop, Status::Regen, Status::Barrier]);
        assert_eq!(s.points_left(Status::Stop), Some(STOP_DURATION));
        assert_eq!(s.points_left(Status::Barrier), Some(BARRIER_DURATION));
        assert_eq!((s.regen_left(), s.points_left(Status::Sleep)), (REGEN_TICKS, None));
        assert!(s.blocks_turns() && !s.silenced());
    }

    /// The Speed Values and V-Timer increases of the FAQ's table (54/91/273 and 108/182/546) and how many ticks a
    /// unit takes at each ("just over 30" at the fastest, 152 at the slowest).
    #[test]
    fn a_unit_of_time_takes_the_number_of_ticks_the_faq_gives() {
        let ticks_for_a_unit = |battle_speed: u8| {
            let v = crate::rules::v_timer_increase(battle_speed);
            let mut points = 0u32;
            let mut ticks = 0u32;
            while points < V_UNIT {
                points += v;
                ticks += 1;
            }
            (v, ticks)
        };
        assert_eq!(ticks_for_a_unit(255), (108, 152));
        assert_eq!(ticks_for_a_unit(128), (182, 91));
        assert_eq!(ticks_for_a_unit(0), (546, 31));
    }

    /// Run `state` for `ticks` ticks at V-Timer increase `v`, returning how many Poison and Regen turns came and the
    /// tick (1-based) at which each ended status ran out.
    fn run(state: &mut StatusState, v: u32, ticks: u32) -> (u32, u32, Vec<(u32, Status)>) {
        let (mut poison, mut regen, mut ended) = (0, 0, Vec::new());
        for tick in 1..=ticks {
            let out = state.advance(v);
            poison += u32::from(out.poison);
            regen += u32::from(out.regen);
            ended.extend(out.ended.iter().map(|s| (tick, s)));
        }
        (poison, regen, ended)
    }

    #[test]
    fn poison_hurts_every_two_and_a_half_units_and_never_ends_by_itself() {
        let v = crate::rules::v_timer_increase(128); // 182 per tick: 225 ticks per 2.5 units
        let mut s = StatusState::starting(StatusSet::EMPTY.with(Status::Poison));
        let (poison, regen, ended) = run(&mut s, v, 226 * 10);
        assert_eq!((poison, regen, ended.len()), (10, 0, 0));
        // 40960 points of the V-Timer per turn: the 225th tick (182 * 225 = 40950) is just short, the 226th is it.
        let mut s = StatusState::starting(StatusSet::EMPTY.with(Status::Poison));
        assert_eq!(run(&mut s, v, 225).0, 0);
        assert_eq!(run(&mut s, v, 1).0, 1);
        assert!(s.has(Status::Poison));
    }

    #[test]
    fn regen_heals_every_unit_32_times_and_then_ends() {
        let v = crate::rules::v_timer_increase(128);
        let mut s = StatusState::starting(StatusSet::EMPTY.with(Status::Regen));
        let (poison, regen, ended) = run(&mut s, v, 91 * 40);
        assert_eq!((poison, regen), (0, 32));
        assert_eq!(ended.len(), 1);
        assert_eq!(ended[0].1, Status::Regen);
        // It ends on the tick of its 32nd heal: 32 units of 16384 points, 182 per tick: tick 2886.
        assert_eq!(ended[0].0, 16384 * 32 / 182 + 1);
        assert!(!s.has(Status::Regen) && s.regen_left() == 0);
    }

    #[test]
    fn stop_and_paralysis_run_out_on_the_global_timer_at_15_and_8_units() {
        let v = crate::rules::v_timer_increase(128); // the Global Timer goes up by 91 per tick
        let mut stop = StatusState::starting(StatusSet::EMPTY.with(Status::Stop));
        let (_, _, ended) = run(&mut stop, v, 3000);
        assert_eq!(ended, [(15 * 8192 / 91 + 1, Status::Stop)], "15 units of 8192 points at 91 per tick");
        let mut paralysed = StatusState::starting(StatusSet::EMPTY.with(Status::Paralysis));
        let (_, _, ended) = run(&mut paralysed, v, 3000);
        assert_eq!(ended, [(8 * 8192 / 91 + 1, Status::Paralysis)]);
        // Haste and Slow do not change how long they last (they are not timed by the pace they freeze).
        let mut hasted = StatusState::starting(StatusSet::EMPTY.with(Status::Haste).with(Status::Stop));
        let (_, _, ended) = run(&mut hasted, v, 3000);
        assert_eq!(ended, [(15 * 8192 / 91 + 1, Status::Stop)]);
    }

    #[test]
    fn haste_doubles_and_slow_halves_how_fast_the_timed_statuses_run_and_stop_freezes_them() {
        let v = crate::rules::v_timer_increase(128);
        let barrier_ticks = |extra: Option<Status>| {
            let mut s = StatusState::starting(StatusSet::EMPTY.with(Status::Barrier));
            if let Some(extra) = extra {
                s.apply(&StatusEffect::inflict(extra, 63), StatusSet::EMPTY);
            }
            let (_, _, ended) = run(&mut s, v, 10_000);
            ended.first().map(|(tick, _)| *tick)
        };
        let normal = barrier_ticks(None).expect("it ends");
        assert_eq!(normal, BARRIER_DURATION / 182 + 1, "30 units at 182 points a tick");
        let hasted = barrier_ticks(Some(Status::Haste)).expect("it ends");
        let slowed = barrier_ticks(Some(Status::Slow)).expect("it ends");
        assert!(hasted.abs_diff(normal / 2) <= 1, "Haste: {hasted} vs half of {normal}");
        assert!(slowed.abs_diff(normal * 2) <= 1, "Slow: {slowed} vs twice {normal}");
        // Stop freezes the clock: the Barrier does not move until Stop is over (15 units of the Global Timer).
        let mut s = StatusState::starting(StatusSet::EMPTY.with(Status::Barrier).with(Status::Stop));
        let (_, _, ended) = run(&mut s, v, 1000);
        assert_eq!(ended.len(), 0, "Stop lasts 1350 ticks here");
        assert_eq!(s.points_left(Status::Barrier), Some(BARRIER_DURATION), "frozen");
        let (_, _, ended) = run(&mut s, v, 400);
        assert_eq!(ended.iter().map(|(_, st)| *st).collect::<Vec<_>>(), [Status::Stop]);
        assert!(s.points_left(Status::Barrier).unwrap() < BARRIER_DURATION, "it runs again");
    }

    #[test]
    fn poison_under_haste_hurts_twice_as_often_and_not_at_all_while_stopped() {
        let v = crate::rules::v_timer_increase(128);
        let count = |extra: Status| {
            let mut s = StatusState::starting(StatusSet::EMPTY.with(Status::Poison).with(extra));
            run(&mut s, v, 226 * 4).0
        };
        assert_eq!(count(Status::Haste), 8);
        assert_eq!(count(Status::Slow), 2);
        assert_eq!(count(Status::Stop), 0);
        assert_eq!(count(Status::Paralysis), 0);
    }

    #[test]
    fn sleep_runs_on_the_combatants_clock_and_a_blow_wakes_it() {
        let v = crate::rules::v_timer_increase(128);
        let mut s = StatusState::starting(StatusSet::EMPTY.with(Status::Sleep));
        assert!(s.blocks_turns());
        assert!(s.wake());
        assert!(!s.has(Status::Sleep) && !s.wake());
        let mut s = StatusState::starting(StatusSet::EMPTY.with(Status::Sleep));
        let (_, _, ended) = run(&mut s, v, 5000);
        assert_eq!(ended, [(SLEEP_DURATION / 182 + 1, Status::Sleep)]);
        assert!(!s.blocks_turns());
    }

    #[test]
    fn a_status_inflicted_again_refills_its_timer_and_a_knockout_clears_everything() {
        let v = crate::rules::v_timer_increase(128);
        let mut s = StatusState::starting(StatusSet::EMPTY.with(Status::Barrier));
        run(&mut s, v, 1000);
        assert!(s.points_left(Status::Barrier).unwrap() < BARRIER_DURATION);
        assert_eq!(s.apply(&StatusEffect::inflict(Status::Barrier, 63), StatusSet::EMPTY), [], "not inflicted again");
        assert_eq!(s.points_left(Status::Barrier), Some(BARRIER_DURATION), "but refilled");
        s.apply(&StatusEffect::inflict(Status::Poison, 63), StatusSet::EMPTY);
        s.clear();
        assert!(s.is_empty() && s == StatusState::none());
    }

    #[test]
    fn the_modifiers_follow_the_documented_numbers() {
        let none = StatusState::none();
        let with = |s: Status| StatusState::starting(StatusSet::EMPTY.with(s));
        // Damage: Barrier halves a physical blow and not a magical one, MBarrier the other way round, Sadness 30% off.
        assert_eq!((damage_after_statuses(100, true, &none), damage_after_statuses(100, true, &with(Status::Barrier))), (100, 50));
        assert_eq!(damage_after_statuses(100, false, &with(Status::Barrier)), 100);
        assert_eq!((damage_after_statuses(100, false, &with(Status::MBarrier)), damage_after_statuses(100, true, &with(Status::MBarrier))), (50, 100));
        assert_eq!(damage_after_statuses(100, true, &with(Status::Sadness)), 70);
        let both = StatusState::starting(StatusSet::EMPTY.with(Status::Barrier).with(Status::Sadness));
        assert_eq!(damage_after_statuses(100, true, &both), 35);
        assert_eq!(damage_after_statuses(1, true, &with(Status::Barrier)), 1, "a hit never does less than 1");
        // Hit chance: Fury x0.7 for everybody, Darkness /2 for a party member's Attack only.
        assert_eq!(hit_chance_after_statuses(100, &with(Status::Fury), false, false), 70);
        assert_eq!(hit_chance_after_statuses(100, &with(Status::Darkness), true, true), 50);
        assert_eq!(hit_chance_after_statuses(100, &with(Status::Darkness), true, false), 100, "an enemy is not blinded");
        assert_eq!(hit_chance_after_statuses(100, &with(Status::Darkness), false, true), 100, "only the weapon commands");
        assert_eq!(hit_chance_after_statuses(100, &none, true, true), 100);
        // Limit gauge: nothing while Paralysed, half under Sadness, double under Fury.
        assert_eq!(
            [Status::Paralysis, Status::Sadness, Status::Fury].map(|s| limit_gain_after_statuses(40, &with(s))),
            [0, 20, 80]
        );
        assert_eq!(limit_gain_after_statuses(40, &none), 40);
    }

    #[test]
    fn pace_scales_gauges_and_clocks() {
        assert_eq!([Pace::Frozen, Pace::Slow, Pace::Normal, Pace::Haste].map(|p| p.scale(182)), [0, 91, 182, 364]);
        assert_eq!(Pace::Haste.scale(u32::MAX), u32::MAX, "no overflow");
        assert_eq!(StatusState::starting(StatusSet::EMPTY.with(Status::Haste).with(Status::Stop)).pace(), Pace::Frozen);
    }
}
