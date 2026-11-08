//! Deterministic, server-authoritative FF7-style battle rules (milestone M2-c subset).
//!
//! The engine is a pure state machine: [`BattleState`] holds the combatants, the ATB gauges
//! and a seeded [`Rng`]; it advances only through [`BattleState::step`] /
//! [`BattleState::step_ticks`] and [`BattleState::submit_command`]. It never reads a clock, a
//! thread-local, the OS RNG or floating point, so a server and any client that replays the
//! same `(seed, party, enemies, config)` plus the same ordered inputs produce identical
//! events. The server stays authoritative: clients should treat server events as truth and
//! use local replay only for prediction.
//!
//! # Scope
//!
//! Two sides: a *party* (one or more player characters) against *enemies*. The only command
//! is [`Command::Attack`] (a single-target physical weapon attack). Enemies use a simple AI:
//! when their gauge fills they attack a random living party member. Not modelled: statuses
//! (Berserk, Defend, Sadness, Fury, ...), elements and affinities, magic, items, limit
//! breaks, escape, back-attack damage bonus, covering, enemy rows, AI scripts, AP/EXP growth.
//!
//! # Formula sources and provenance
//!
//! * **ATB gauge fill rate.** From the FF7 Project blog's "Stats & Battle Timers"
//!   (<https://finalfantasy7project.wordpress.com/2011/11/24/stats/>), which credits Terence
//!   Fergusson's *FF7 Battle Mechanics* FAQ
//!   (<https://gamefaqs.gamespot.com/ps/197341-final-fantasy-vii/faqs/22395>; the FAQ itself
//!   could not be fetched, so this crate relies on the blog's quotation of it):
//!   `speed_value = 32768 / (120 + battle_speed * 15 / 8)`, `v_timer_increase = 2 * speed_value`,
//!   `normal_speed = round(average party Dex) + 50`; party members add
//!   `(Dex + 50) * v_timer_increase / normal_speed` to their turn timer every tick, enemies
//!   `Dex * v_timer_increase / normal_speed`; a combatant can act when the timer reaches 65535.
//!   A pastebin note, "FF7 Battle Timer" (<https://pastebin.com/nVBvNNfP>), restates the
//!   timer definitions (tick, global timer, V-timer, turn timer normalised by the party's
//!   average *base* Dexterity, C-timer). Secondary sources, therefore treat the ATB rates as
//!   *documented but not independently verified*; this crate uses base Dexterity only
//!   (no Speed Plus / Choco Feather style modifiers).
//! * **Physical damage ("X1" formula), hit chance and critical chance.** From the
//!   disassembly-derived pseudocode at <https://wiki.ffrtt.ru/index.php/FF7/DamageFormula>
//!   and the formula table at <https://wiki.ffrtt.ru/index.php/FF7/Battle/Damage_Calculation>
//!   (English 1.02 build), cross-checked against the commonly quoted form
//!   `Base = Att + ((Att + Lvl) / 32) * ((Att * Lvl) / 32)`,
//!   `Damage = Power * (512 - Def) * Base / (16 * 512)`:
//!   critical x2, short-range row penalty /2, then random variance `[3841..=4096] / 4096`
//!   with a minimum of 1.
//! * **Row rule**: <https://wiki.ffrtt.ru/index.php/FF7/Battle/Targeting_Data> (bit `0x20`,
//!   "short range").
//!
//! # RNG draw order (part of the determinism contract)
//!
//! * Construction, [`StartMode::Normal`] only: one `range_inclusive(0, 0x7FFF)` per combatant
//!   in [`ActorId`] order (party first, then enemies) for the initial gauge.
//! * Enemy turn: first `below(living_party_count)` to pick the target (living party members
//!   in id order), then the attack draws below.
//! * Each attack, in this order: `luck_roll = range_inclusive(0, 99)`;
//!   `miss_roll = next_u16() * 99 / 65535 + 1`; **only if it hits**
//!   `crit_roll = next_u16() * 99 / 65535 + 1`, then `variance = range_inclusive(0, 255)`.
//!
//! # APPROXIMATION summary
//!
//! * **Tick duration**: the documents give per-tick increments but not how long a tick is.
//!   [`DEFAULT_TICKS_PER_SECOND`] = 60 (a default-speed, average-Dex combatant then needs a
//!   little over six seconds to fill). Configurable through [`BattleConfig`].
//! * **Initial gauges**: not documented; [`StartMode::Normal`] draws a random fill in
//!   `0..=0x7FFF` (at most half full). Preemptive starts with a full party gauge and an
//!   empty enemy gauge, back attack the reverse (APPROXIMATION from the layout names).
//!   Layouts other than preemptive / back attack are treated as normal.
//! * **Critical test**: `chance >= roll` as in the FAQ (an exact `chance`% rate); the
//!   disassembly pseudocode uses a strict `>`, a difference of one roll value.
//! * **Damage cap** [`MAX_DAMAGE`] = 9999 (community-documented, not in the cited pages).
//!   Arithmetic is exact 64-bit; the original's 32-bit overflow glitch is not reproduced.
//! * **Enemy attack**: the enemy's first plain physical scene attack supplies Power and
//!   hit rate (see [`Combatant::from_enemy`]); the original picks attacks with AI scripts.
//!   The Enemy Attack command is always row-sensitive (`short_range = true`).
//! * **Party attack and defense** = Strength + weapon attack, Vitality + armor defense; the
//!   weapon's hit/crit rates and the armor's evade percentage come from the kernel. Cait Sith
//!   and Vincent have no starting equipment record, so they fight unarmed unless equipment
//!   ids are passed explicitly.
//! * Enemy rows from formations are ignored; party rows are honoured.
//!
//! # Events, not state, go on the wire
//!
//! [`BattleEvent`], [`Command`], [`ActorId`] and [`Outcome`] derive serde so they can travel in
//! protocol messages. [`Combatant`] and [`BattleState`] deliberately do not: they carry
//! numbers derived from the owner's game files, which are never sent over the network.

use serde::{Deserialize, Serialize};

use crate::kernel::{CharacterId, Kernel};
use crate::rng::Rng;
use crate::scene::{AttackRecord, BattleLayout, EnemyRecord, Scene};

/// Turn-timer value at which a combatant can act.
pub const GAUGE_MAX: u32 = 65_535;
/// Default battle speed setting (0 fastest .. 255 slowest).
pub const DEFAULT_BATTLE_SPEED: u8 = 128;
/// APPROXIMATION: ticks per second (see the module docs).
pub const DEFAULT_TICKS_PER_SECOND: u32 = 60;
/// Largest `dt_ms` a single [`BattleState::step`] call will consume; the rest is dropped.
pub const MAX_STEP_MS: u32 = 60_000;
/// Largest tick count a single [`BattleState::step_ticks`] call will process.
pub const MAX_STEP_TICKS: u64 = 60_000;
/// Combatants kept per side by [`BattleState::new`] (extras are dropped).
pub const MAX_PER_SIDE: usize = 32;
/// Power of the Attack command.
pub const ATTACK_COMMAND_POWER: u32 = 16;
/// Largest damage one hit can deal.
pub const MAX_DAMAGE: u32 = 9_999;
/// APPROXIMATION: upper bound of the random initial gauge of a normal start.
pub const INITIAL_GAUGE_MAX: u32 = 0x7FFF;

/// Identifier of a combatant inside one battle: party members take ids `0..party`, enemies
/// follow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ActorId(pub u8);

/// Which side a combatant fights on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    Party,
    Enemy,
}

/// Battle row. Back-row combatants deal and take half damage from short-range attacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Row {
    #[default]
    Front,
    Back,
}

/// Rewards granted for defeating an enemy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Reward {
    pub exp: u32,
    pub gil: u32,
    pub ap: u32,
}

/// One fighter. All stats use the units of FF7's battle formulas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Combatant {
    pub name: String,
    pub level: u8,
    pub hp: u32,
    pub max_hp: u32,
    pub mp: u32,
    pub max_mp: u32,
    /// "Att" of the damage formula (party: Strength + weapon attack; enemy: Strength).
    pub attack: u32,
    /// "Def" of the damage formula (party: Vitality + armor defense; enemy: Defense).
    pub defense: u32,
    /// Dexterity (an enemy's speed byte): drives the ATB gauge and hit chance.
    pub dex: u32,
    pub luck: u32,
    /// Physical evade percentage (party: armor "Defense %"; enemy: evade stat).
    pub evade: u32,
    /// Hit rate (percent) of the Attack action (weapon accuracy or the enemy attack's rate).
    pub accuracy: u32,
    /// Power of the Attack action (16 for the party's Attack command).
    pub attack_power: u32,
    /// Critical rate bonus in percent (party weapons only).
    pub crit_bonus: u32,
    /// Short-range attacks are halved when either fighter is in the back row.
    pub short_range: bool,
    pub row: Row,
    /// Rewards for defeating this combatant (enemies).
    pub reward: Reward,
    /// Assigned by [`BattleState`]; any value passed in is overwritten.
    pub side: Side,
    /// Turn timer, `0..=`[`GAUGE_MAX`]. Runtime state owned by [`BattleState`].
    pub gauge: u32,
    /// True once the gauge is full and the combatant has not yet acted.
    pub ready: bool,
}

impl Combatant {
    /// A combatant with neutral defaults: no attack/defense/dex, 100% accuracy, Power 16,
    /// front row, short range. Set the public fields to taste.
    pub fn new(name: impl Into<String>, level: u8, max_hp: u32) -> Self {
        Self {
            name: name.into(),
            level,
            hp: max_hp,
            max_hp,
            mp: 0,
            max_mp: 0,
            attack: 0,
            defense: 0,
            dex: 0,
            luck: 0,
            evade: 0,
            accuracy: 100,
            attack_power: ATTACK_COMMAND_POWER,
            crit_bonus: 0,
            short_range: true,
            row: Row::Front,
            reward: Reward::default(),
            side: Side::Party,
            gauge: 0,
            ready: false,
        }
    }

    pub fn is_alive(&self) -> bool {
        self.hp > 0
    }

    /// Build an enemy from its scene record. `basic_attack` (see [`Scene::basic_attack`])
    /// supplies the Attack action's Power and hit rate; without it the enemy uses Power 16 at
    /// 100%.
    pub fn from_enemy(record: &EnemyRecord, basic_attack: Option<&AttackRecord>) -> Self {
        let mut c = Combatant::new(record.name.clone(), record.level, record.hp);
        c.side = Side::Enemy;
        c.mp = u32::from(record.mp);
        c.max_mp = u32::from(record.mp);
        c.attack = u32::from(record.strength);
        c.defense = u32::from(record.defense);
        c.dex = u32::from(record.speed);
        c.luck = u32::from(record.luck);
        c.evade = u32::from(record.evade);
        if let Some(a) = basic_attack {
            c.accuracy = u32::from(a.accuracy);
            c.attack_power = u32::from(a.power);
        }
        c.short_range = true; // the Enemy Attack command is always row-sensitive
        c.reward = Reward {
            exp: record.exp,
            gil: record.gil,
            ap: u32::from(record.ap),
        };
        c
    }

    /// Build a party member from kernel data: a deterministic stat block for `level`
    /// ([`crate::kernel::GrowthData::stat_block`]) plus equipment.
    ///
    /// `weapon` / `armor` override the equipment ids; `None` uses the character's starting
    /// equipment from the kernel initialization data, and a character with no initialization
    /// record (Cait Sith, Vincent) is unarmed and unarmored. Returns `None` if the kernel
    /// has no growth curves for the character.
    pub fn from_character(
        kernel: &Kernel,
        who: CharacterId,
        level: u8,
        weapon: Option<u8>,
        armor: Option<u8>,
    ) -> Option<Self> {
        let stats = kernel.growth.stat_block(who, level)?;
        let init = kernel.init_character_by_id(who.index() as u8);
        let weapon = weapon
            .or(init.map(|i| i.weapon))
            .and_then(|id| kernel.weapon(id));
        let armor = armor
            .or(init.map(|i| i.armor))
            .and_then(|id| kernel.armor(id));

        let mut c = Combatant::new(who.name(), stats.level, u32::from(stats.max_hp));
        c.mp = u32::from(stats.max_mp);
        c.max_mp = u32::from(stats.max_mp);
        c.attack = u32::from(stats.strength) + weapon.map_or(0, |w| u32::from(w.attack));
        c.defense = u32::from(stats.vitality) + armor.map_or(0, |a| u32::from(a.defense));
        c.dex = u32::from(stats.dexterity);
        c.luck = u32::from(stats.luck);
        c.evade = armor.map_or(0, |a| u32::from(a.evade_percent));
        if let Some(w) = weapon {
            c.accuracy = u32::from(w.accuracy);
            c.crit_bonus = u32::from(w.crit_rate);
            c.short_range = w.is_short_range();
        }
        c.row = if init.is_some_and(|i| i.back_row) {
            Row::Back
        } else {
            Row::Front
        };
        Some(c)
    }
}

/// The enemies of formation `formation_slot` (0..4) of `scene`, in placement order. Empty if
/// the slot is out of range or the formation has no resolvable enemies.
pub fn enemies_for_formation(scene: &Scene, formation_slot: usize) -> Vec<Combatant> {
    scene
        .formation_enemies(formation_slot)
        .into_iter()
        .map(|r| {
            let attack = scene.basic_attack(r.enemy_slot).map(|a| &a.record);
            Combatant::from_enemy(r.record, attack)
        })
        .collect()
}

/// How the gauges are initialised.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StartMode {
    /// Every gauge starts at a random fill in `0..=`[`INITIAL_GAUGE_MAX`] (APPROXIMATION).
    #[default]
    Normal,
    /// Party gauges full, enemy gauges empty.
    Preemptive,
    /// Enemy gauges full, party gauges empty.
    BackAttack,
    /// All gauges start empty (deterministic pace; used by tests).
    Zero,
}

impl StartMode {
    /// Map a scene battle layout to a start mode (see the module docs).
    pub fn from_layout(layout: BattleLayout) -> Self {
        match layout {
            BattleLayout::Preemptive => StartMode::Preemptive,
            BattleLayout::BackAttack => StartMode::BackAttack,
            _ => StartMode::Normal,
        }
    }
}

/// Tunable parameters of a battle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BattleConfig {
    /// FF7 "Battle Speed" configuration value: 0 is fastest, 255 slowest.
    pub battle_speed: u8,
    /// Ticks per second of simulated time (clamped to `1..=1000`).
    pub ticks_per_second: u32,
    pub start: StartMode,
}

impl Default for BattleConfig {
    fn default() -> Self {
        Self {
            battle_speed: DEFAULT_BATTLE_SPEED,
            ticks_per_second: DEFAULT_TICKS_PER_SECOND,
            start: StartMode::Normal,
        }
    }
}

/// A command a party member can issue when its gauge is full.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Command {
    /// Physical weapon attack on one living enemy.
    Attack { target: ActorId },
}

/// How the battle ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome {
    Victory,
    Defeat,
}

/// Something that happened in the battle, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BattleEvent {
    /// `actor`'s gauge filled (party members now wait for a command; enemies act at once).
    GaugeReady { actor: ActorId },
    /// A hit. `amount` is the damage rolled; the target's HP drops by at most that much.
    Damage {
        actor: ActorId,
        target: ActorId,
        amount: u32,
        critical: bool,
    },
    /// The attack missed. (Addition to the minimal event set: hit chance is modelled.)
    Miss { actor: ActorId, target: ActorId },
    /// `actor` was reduced to 0 HP.
    Death { actor: ActorId },
    /// All enemies are dead.
    Victory,
    /// All party members are dead.
    Defeat,
}

/// Why a command was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CommandError {
    #[error("the battle is over")]
    BattleOver,
    #[error("unknown actor {0:?}")]
    UnknownActor(ActorId),
    #[error("actor {0:?} is not party-controlled")]
    NotControllable(ActorId),
    #[error("actor {0:?} is dead")]
    ActorDead(ActorId),
    #[error("actor {0:?} has no full gauge")]
    GaugeNotFull(ActorId),
    #[error("unknown target {0:?}")]
    UnknownTarget(ActorId),
    #[error("target {0:?} is not a valid target (attacks must target an enemy)")]
    InvalidTarget(ActorId),
    #[error("target {0:?} is already dead")]
    TargetDead(ActorId),
}

/// Inputs of the physical damage formula (all roll results are passed in).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DamageInput {
    pub attack: u32,
    pub level: u32,
    pub power: u32,
    pub defense: u32,
    pub critical: bool,
    /// Short-range attack with a back-row attacker or target: damage is halved.
    pub halve_for_row: bool,
    /// Random variance roll `0..=255` (damage is multiplied by `(3841 + roll) / 4096`).
    pub variance_roll: u32,
}

/// The physical ("X1") damage formula, in the order of the disassembly:
/// `Base = Att + ((Att + Lvl) / 32) * ((Att * Lvl) / 32)`,
/// `damage = Power * (512 - Def) * Base / (16 * 512)`, critical x2, row penalty /2, then
/// variance `(3841 + roll) / 4096` with a minimum of 1, capped at [`MAX_DAMAGE`].
pub fn physical_damage(input: &DamageInput) -> u32 {
    // Real stats are tiny (attack <= ~510, level <= 255, power <= 255); the clamps only keep
    // absurd hand-made inputs from overflowing the 64-bit arithmetic.
    let att = u64::from(input.attack.min(0xFFFF));
    let lvl = u64::from(input.level.min(255));
    let power = u64::from(input.power.min(0xFFFF));
    let x = (lvl * att) / 32;
    let y = (lvl + att) / 32;
    let base = att + x * y;
    let defense = u64::from(input.defense).min(511);
    let mut dmg = (base * (512 - defense) * power) >> 13;
    if input.critical {
        dmg *= 2;
    }
    if input.halve_for_row {
        dmg /= 2;
    }
    dmg = (dmg * (3841 + u64::from(input.variance_roll.min(255)))) >> 12;
    dmg.max(1).min(u64::from(MAX_DAMAGE)) as u32
}

/// Base hit chance (before luck) of a physical attack, as an unclamped percentage:
/// `(Dex/4 + accuracy) + attacker_evade - target_evade`, where a party member's evade is
/// `Dex/4 + armor evade%` and an enemy's is its evade stat. A result of exactly 0 becomes 1.
pub fn hit_chance(actor: &Combatant, target: &Combatant) -> i32 {
    let evade_of = |c: &Combatant| -> i64 {
        match c.side {
            Side::Enemy => i64::from(c.evade),
            Side::Party => i64::from(c.dex >> 2) + i64::from(c.evade),
        }
    };
    let physical = i64::from(actor.dex >> 2) + i64::from(actor.accuracy);
    let chance = physical + evade_of(actor) - evade_of(target);
    let chance = chance.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
    if chance == 0 {
        1
    } else {
        chance
    }
}

/// Decide a physical hit from the chance and the three rolls.
///
/// `luck_roll` is `0..=99`, `miss_roll` is `1..=100`. A "lucky hit" (attacker `Luck / 4 >
/// luck_roll`) always connects; a party attacker against an enemy can instead hit a "lucky
/// dodge" (target `Luck / 4 > luck_roll`) and always misses. Otherwise the attack hits when
/// `chance >= miss_roll`.
pub fn resolve_hit(
    actor: &Combatant,
    target: &Combatant,
    chance: i32,
    luck_roll: u32,
    miss_roll: u32,
) -> bool {
    let mut chance = i64::from(chance);
    if actor.luck / 4 > luck_roll {
        chance = 255;
    } else if actor.side == Side::Party && target.side == Side::Enemy && target.luck / 4 > luck_roll
    {
        chance = 0;
    }
    chance >= i64::from(miss_roll)
}

/// Critical chance in percent: `((Luck + Lvl) - target Lvl) >> 2`, plus the weapon crit
/// bonus for party attackers; never negative.
pub fn critical_chance(actor: &Combatant, target: &Combatant) -> u32 {
    let base = (i64::from(actor.luck) + i64::from(actor.level) - i64::from(target.level)) >> 2;
    let bonus = match actor.side {
        Side::Party => i64::from(actor.crit_bonus),
        Side::Enemy => 0,
    };
    (base + bonus).clamp(0, 255) as u32
}

/// Map a raw 16-bit roll to `1..=100` like FF7's `rand(0..=65535) * 99 / 65535 + 1`.
fn percent_roll(raw: u16) -> u32 {
    u32::from(raw) * 99 / 65_535 + 1
}

/// Gauge increase per tick for a combatant (see the module docs for the formula).
pub fn turn_timer_increase(side: Side, dex: u32, v_timer_increase: u32, normal_speed: u32) -> u32 {
    // Real values are tiny (Dex <= ~255, increase <= 546); the clamps only keep absurd
    // hand-made inputs from overflowing.
    let dex = u64::from(dex.min(0xFFFF));
    let v_timer_increase = u64::from(v_timer_increase.min(0xFFFF));
    let numerator = match side {
        Side::Party => dex + 50,
        Side::Enemy => dex,
    } * v_timer_increase;
    let inc = numerator / u64::from(normal_speed.max(1));
    inc.min(u64::from(GAUGE_MAX)) as u32
}

/// `v_timer_increase = 2 * (32768 / (120 + battle_speed * 15 / 8))`.
pub fn v_timer_increase(battle_speed: u8) -> u32 {
    2 * (32_768 / (120 + u32::from(battle_speed) * 15 / 8))
}

/// A running battle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BattleState {
    config: BattleConfig,
    rng: Rng,
    actors: Vec<Combatant>,
    party_len: usize,
    tick: u64,
    /// Milliseconds-times-ticks-per-second not yet converted to whole ticks.
    clock_remainder: u64,
    v_timer_increase: u32,
    normal_speed: u32,
    outcome: Option<Outcome>,
    /// Events produced at construction, delivered with the next step or command.
    pending: Vec<BattleEvent>,
}

impl BattleState {
    /// Start a battle with the default [`BattleConfig`].
    ///
    /// Party members become actors `0..party.len()` and enemies follow. Each side is
    /// truncated to [`MAX_PER_SIDE`]. If a side starts with nobody alive, the outcome is set
    /// immediately (enemies are checked first, so an empty enemy list is a victory) and the
    /// matching event is delivered with the first step.
    pub fn new(seed: u64, party: Vec<Combatant>, enemies: Vec<Combatant>) -> Self {
        Self::with_config(seed, party, enemies, BattleConfig::default())
    }

    /// Start a battle with an explicit configuration.
    pub fn with_config(
        seed: u64,
        mut party: Vec<Combatant>,
        mut enemies: Vec<Combatant>,
        mut config: BattleConfig,
    ) -> Self {
        config.ticks_per_second = config.ticks_per_second.clamp(1, 1000);
        party.truncate(MAX_PER_SIDE);
        enemies.truncate(MAX_PER_SIDE);
        let party_len = party.len();

        let mut actors: Vec<Combatant> = Vec::with_capacity(party_len + enemies.len());
        for mut c in party {
            c.side = Side::Party;
            actors.push(c);
        }
        for mut c in enemies {
            c.side = Side::Enemy;
            actors.push(c);
        }

        // Normal speed: rounded average Dex of the party, plus 50.
        let dex_sum: u64 = actors
            .iter()
            .take(party_len)
            .map(|c| u64::from(c.dex))
            .sum();
        let n = party_len.max(1) as u64;
        let normal_speed = (((dex_sum + n / 2) / n) as u32).saturating_add(50);

        let mut rng = Rng::new(seed);
        for c in &mut actors {
            c.hp = c.hp.min(c.max_hp);
            c.gauge = match (config.start, c.side) {
                (StartMode::Normal, _) => rng.range_inclusive(0, INITIAL_GAUGE_MAX),
                (StartMode::Preemptive, Side::Party) | (StartMode::BackAttack, Side::Enemy) => {
                    GAUGE_MAX
                }
                _ => 0,
            };
            c.ready = false;
        }

        let mut state = Self {
            config,
            rng,
            actors,
            party_len,
            tick: 0,
            clock_remainder: 0,
            v_timer_increase: v_timer_increase(config.battle_speed),
            normal_speed,
            outcome: None,
            pending: Vec::new(),
        };
        for i in 0..state.actors.len() {
            let c = &mut state.actors[i];
            if c.is_alive() && c.gauge >= GAUGE_MAX {
                c.gauge = GAUGE_MAX;
                c.ready = true;
                state.pending.push(BattleEvent::GaugeReady {
                    actor: ActorId(i as u8),
                });
            }
        }
        let mut initial = Vec::new();
        state.check_outcome(&mut initial);
        state.pending.extend(initial);
        state
    }

    // --- accessors ---

    pub fn config(&self) -> &BattleConfig {
        &self.config
    }

    /// All combatants; index with [`ActorId`]`.0`.
    pub fn actors(&self) -> &[Combatant] {
        &self.actors
    }

    pub fn actor(&self, id: ActorId) -> Option<&Combatant> {
        self.actors.get(usize::from(id.0))
    }

    pub fn party_ids(&self) -> Vec<ActorId> {
        (0..self.party_len).map(|i| ActorId(i as u8)).collect()
    }

    pub fn enemy_ids(&self) -> Vec<ActorId> {
        (self.party_len..self.actors.len())
            .map(|i| ActorId(i as u8))
            .collect()
    }

    /// Party members that are alive and waiting for a command.
    pub fn ready_actors(&self) -> Vec<ActorId> {
        (0..self.party_len)
            .filter(|&i| self.actors[i].is_alive() && self.actors[i].ready)
            .map(|i| ActorId(i as u8))
            .collect()
    }

    /// Whole ticks simulated so far.
    pub fn tick(&self) -> u64 {
        self.tick
    }

    pub fn outcome(&self) -> Option<Outcome> {
        self.outcome
    }

    /// Gauge increase per tick of `id` (for pacing displays and tests).
    pub fn gauge_increase(&self, id: ActorId) -> Option<u32> {
        let c = self.actor(id)?;
        Some(turn_timer_increase(
            c.side,
            c.dex,
            self.v_timer_increase,
            self.normal_speed,
        ))
    }

    /// The party's "normal speed" (rounded average Dex + 50) used to scale every gauge.
    pub fn normal_speed(&self) -> u32 {
        self.normal_speed
    }

    /// Sum of the rewards of all dead enemies (meaningful once the outcome is `Victory`).
    pub fn rewards(&self) -> Reward {
        let mut total = Reward::default();
        for c in self
            .actors
            .iter()
            .skip(self.party_len)
            .filter(|c| !c.is_alive())
        {
            total.exp = total.exp.saturating_add(c.reward.exp);
            total.gil = total.gil.saturating_add(c.reward.gil);
            total.ap = total.ap.saturating_add(c.reward.ap);
        }
        total
    }

    // --- stepping ---

    /// Advance simulated time by `dt_ms` milliseconds (at most [`MAX_STEP_MS`]; the rest is
    /// dropped) and return what happened. Time is converted to whole ticks with an exact
    /// integer accumulator, so many small steps equal one big step.
    pub fn step(&mut self, dt_ms: u32) -> Vec<BattleEvent> {
        let dt = u64::from(dt_ms.min(MAX_STEP_MS));
        let tps = u64::from(self.config.ticks_per_second);
        self.clock_remainder += dt * tps;
        let ticks = self.clock_remainder / 1000;
        self.clock_remainder %= 1000;
        self.step_ticks(ticks)
    }

    /// Advance by whole ticks (at most [`MAX_STEP_TICKS`]) and return what happened.
    /// Stops early once the battle has an outcome.
    pub fn step_ticks(&mut self, ticks: u64) -> Vec<BattleEvent> {
        let mut events = Vec::new();
        self.flush_pending(&mut events);
        for _ in 0..ticks.min(MAX_STEP_TICKS) {
            if self.outcome.is_some() {
                break;
            }
            self.tick_once(&mut events);
        }
        events
    }

    /// Submit a command for a party member whose gauge is full. On success the command is
    /// resolved immediately and the resulting events (preceded by any undelivered start
    /// events) are returned; the actor's gauge resets to empty.
    pub fn submit_command(
        &mut self,
        actor: ActorId,
        command: Command,
    ) -> Result<Vec<BattleEvent>, CommandError> {
        if self.outcome.is_some() {
            return Err(CommandError::BattleOver);
        }
        let idx = usize::from(actor.0);
        let me = self
            .actors
            .get(idx)
            .ok_or(CommandError::UnknownActor(actor))?;
        if idx >= self.party_len {
            return Err(CommandError::NotControllable(actor));
        }
        if !me.is_alive() {
            return Err(CommandError::ActorDead(actor));
        }
        if !me.ready {
            return Err(CommandError::GaugeNotFull(actor));
        }
        match command {
            Command::Attack { target } => {
                let t_idx = usize::from(target.0);
                let victim = self
                    .actors
                    .get(t_idx)
                    .ok_or(CommandError::UnknownTarget(target))?;
                if t_idx < self.party_len {
                    return Err(CommandError::InvalidTarget(target));
                }
                if !victim.is_alive() {
                    return Err(CommandError::TargetDead(target));
                }
                let mut events = Vec::new();
                self.flush_pending(&mut events);
                self.resolve_attack(idx, t_idx, &mut events);
                self.finish_turn(idx);
                Ok(events)
            }
        }
    }

    // --- internals ---

    fn flush_pending(&mut self, out: &mut Vec<BattleEvent>) {
        out.append(&mut self.pending);
    }

    /// One tick: fill gauges, then let ready enemies act (in id order).
    fn tick_once(&mut self, events: &mut Vec<BattleEvent>) {
        self.tick += 1;
        for i in 0..self.actors.len() {
            let c = &self.actors[i];
            if !c.is_alive() || c.ready {
                continue;
            }
            let inc = turn_timer_increase(c.side, c.dex, self.v_timer_increase, self.normal_speed);
            let c = &mut self.actors[i];
            c.gauge = (c.gauge + inc).min(GAUGE_MAX);
            if c.gauge >= GAUGE_MAX {
                c.ready = true;
                events.push(BattleEvent::GaugeReady {
                    actor: ActorId(i as u8),
                });
            }
        }
        for i in self.party_len..self.actors.len() {
            if self.outcome.is_some() {
                break;
            }
            if self.actors[i].is_alive() && self.actors[i].ready {
                self.enemy_act(i, events);
            }
        }
    }

    /// Enemy AI: attack a uniformly random living party member.
    fn enemy_act(&mut self, idx: usize, events: &mut Vec<BattleEvent>) {
        let living: Vec<usize> = (0..self.party_len)
            .filter(|&p| self.actors[p].is_alive())
            .collect();
        if !living.is_empty() {
            let pick = self.rng.below(living.len() as u32) as usize;
            let target = living.get(pick).copied().unwrap_or(0);
            self.resolve_attack(idx, target, events);
        }
        self.finish_turn(idx);
    }

    fn finish_turn(&mut self, idx: usize) {
        if let Some(c) = self.actors.get_mut(idx) {
            c.gauge = 0;
            c.ready = false;
        }
    }

    /// Resolve one physical attack with a fixed RNG draw order (see the module docs).
    fn resolve_attack(&mut self, attacker: usize, target: usize, events: &mut Vec<BattleEvent>) {
        let actor_id = ActorId(attacker as u8);
        let target_id = ActorId(target as u8);
        let luck_roll = self.rng.range_inclusive(0, 99);
        let miss_roll = percent_roll(self.rng.next_u16());

        let (hit, critical_chance_pct, damage_template) = {
            let (Some(a), Some(t)) = (self.actors.get(attacker), self.actors.get(target)) else {
                return;
            };
            let chance = hit_chance(a, t);
            let hit = resolve_hit(a, t, chance, luck_roll, miss_roll);
            let halve = a.short_range && (t.row == Row::Back || a.row == Row::Back);
            (
                hit,
                critical_chance(a, t),
                DamageInput {
                    attack: a.attack,
                    level: u32::from(a.level),
                    power: a.attack_power,
                    defense: t.defense,
                    critical: false,
                    halve_for_row: halve,
                    variance_roll: 0,
                },
            )
        };

        if !hit {
            events.push(BattleEvent::Miss {
                actor: actor_id,
                target: target_id,
            });
            return;
        }

        let crit_roll = percent_roll(self.rng.next_u16());
        let critical = critical_chance_pct >= crit_roll;
        let variance_roll = self.rng.range_inclusive(0, 255);
        let amount = physical_damage(&DamageInput {
            critical,
            variance_roll,
            ..damage_template
        });

        let mut died = false;
        if let Some(t) = self.actors.get_mut(target) {
            t.hp = t.hp.saturating_sub(amount);
            if t.hp == 0 {
                died = true;
                t.gauge = 0;
                t.ready = false;
            }
        }
        events.push(BattleEvent::Damage {
            actor: actor_id,
            target: target_id,
            amount,
            critical,
        });
        if died {
            events.push(BattleEvent::Death { actor: target_id });
        }
        self.check_outcome(events);
    }

    /// Set the outcome if a side has been wiped out (enemies are checked first).
    fn check_outcome(&mut self, events: &mut Vec<BattleEvent>) {
        if self.outcome.is_some() {
            return;
        }
        let enemies_alive = self
            .actors
            .iter()
            .skip(self.party_len)
            .any(Combatant::is_alive);
        let party_alive = self
            .actors
            .iter()
            .take(self.party_len)
            .any(Combatant::is_alive);
        if !enemies_alive {
            self.outcome = Some(Outcome::Victory);
            events.push(BattleEvent::Victory);
        } else if !party_alive {
            self.outcome = Some(Outcome::Defeat);
            events.push(BattleEvent::Defeat);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::Kernel;
    use crate::scene::SceneBin;
    use crate::testutil::{demo_scene, gzip, pack_scene_bin, synthetic_kernel_bytes};

    fn fighter(name: &str, level: u8, hp: u32, attack: u32, defense: u32, dex: u32) -> Combatant {
        let mut c = Combatant::new(name, level, hp);
        c.attack = attack;
        c.defense = defense;
        c.dex = dex;
        c
    }

    fn zero_start() -> BattleConfig {
        BattleConfig {
            start: StartMode::Zero,
            ..BattleConfig::default()
        }
    }

    fn simple_battle(seed: u64, config: BattleConfig) -> BattleState {
        let party = vec![
            fighter("Hero A", 10, 300, 60, 20, 40),
            fighter("Hero B", 10, 280, 55, 18, 30),
        ];
        let mut e1 = fighter("Enemy 1", 8, 120, 30, 10, 50);
        e1.reward = Reward {
            exp: 20,
            gil: 30,
            ap: 2,
        };
        let mut e2 = fighter("Enemy 2", 8, 90, 25, 8, 45);
        e2.reward = Reward {
            exp: 12,
            gil: 18,
            ap: 1,
        };
        BattleState::with_config(seed, party, vec![e1, e2], config)
    }

    /// Drive a battle: step in 100 ms slices; ready party members attack the first living
    /// enemy. Returns every event in order.
    fn play_out(state: &mut BattleState, max_slices: usize) -> Vec<BattleEvent> {
        let mut log = Vec::new();
        for _ in 0..max_slices {
            if state.outcome().is_some() {
                break;
            }
            log.extend(state.step(100));
            for id in state.ready_actors() {
                if state.outcome().is_some() {
                    break;
                }
                let Some(target) = state
                    .enemy_ids()
                    .into_iter()
                    .find(|e| state.actor(*e).is_some_and(Combatant::is_alive))
                else {
                    break;
                };
                if let Ok(ev) = state.submit_command(id, Command::Attack { target }) {
                    log.extend(ev);
                }
            }
        }
        log
    }

    // --- formulas ---

    #[test]
    fn damage_formula_hand_computed() {
        // Att 22, Lvl 6: (28/32)=0 so Base = 22. Power 16, Def 2:
        // 16 * 510 * 22 = 179520; >> 13 = 21.
        let base = DamageInput {
            attack: 22,
            level: 6,
            power: 16,
            defense: 2,
            critical: false,
            halve_for_row: false,
            variance_roll: 255,
        };
        assert_eq!(physical_damage(&base), 21);
        // Variance floor: 21 * 3841 >> 12 = 19.
        assert_eq!(
            physical_damage(&DamageInput {
                variance_roll: 0,
                ..base
            }),
            19
        );
        // Critical doubles before variance; the row penalty halves.
        assert_eq!(
            physical_damage(&DamageInput {
                critical: true,
                ..base
            }),
            42
        );
        assert_eq!(
            physical_damage(&DamageInput {
                halve_for_row: true,
                ..base
            }),
            10
        );
        assert_eq!(
            physical_damage(&DamageInput {
                critical: true,
                halve_for_row: true,
                ..base
            }),
            21
        );

        // Att 120, Lvl 40: x = 4800/32 = 150, y = 160/32 = 5, Base = 120 + 750 = 870.
        // Power 16, Def 50: 870 * 462 * 16 >> 13 = 785.
        let big = DamageInput {
            attack: 120,
            level: 40,
            power: 16,
            defense: 50,
            critical: false,
            halve_for_row: false,
            variance_roll: 255,
        };
        assert_eq!(physical_damage(&big), 785);
        assert_eq!(
            physical_damage(&DamageInput {
                critical: true,
                ..big
            }),
            1570
        );
        // Power scales linearly; the attack command's Power 16 is the reference.
        assert_eq!(physical_damage(&DamageInput { power: 32, ..big }), 1570);

        // Minimum of 1 on any hit, and the 9999 cap.
        assert_eq!(physical_damage(&DamageInput { attack: 0, ..big }), 1);
        assert_eq!(
            physical_damage(&DamageInput {
                defense: 600,
                ..big
            }),
            physical_damage(&DamageInput {
                defense: 511,
                ..big
            })
        );
        let huge = DamageInput {
            attack: 255,
            level: 99,
            power: 255,
            defense: 0,
            ..big
        };
        assert_eq!(physical_damage(&huge), MAX_DAMAGE);
    }

    #[test]
    fn hit_and_critical_formulas() {
        let mut party = fighter("P", 10, 100, 0, 0, 20);
        party.accuracy = 96;
        party.evade = 4;
        party.luck = 12;
        party.side = Side::Party;
        let mut enemy = fighter("E", 6, 100, 0, 0, 40);
        enemy.accuracy = 100;
        enemy.evade = 6;
        enemy.luck = 8;
        enemy.side = Side::Enemy;
        // Party attacker: (20>>2) + 96 = 101; own evade (20>>2)+4 = 9; enemy evade 6 => 104.
        assert_eq!(hit_chance(&party, &enemy), 104);
        // Enemy attacker: (40>>2) + 100 = 110; own evade 6 (stat only); party target evade 9.
        assert_eq!(hit_chance(&enemy, &party), 107);
        // Exactly zero is bumped to 1; negative stays negative.
        let mut weak = fighter("W", 1, 1, 0, 0, 0);
        weak.accuracy = 9;
        weak.evade = 0;
        weak.side = Side::Enemy;
        let mut sturdy = fighter("S", 1, 1, 0, 0, 0);
        sturdy.evade = 9;
        sturdy.side = Side::Enemy;
        assert_eq!(hit_chance(&weak, &sturdy), 1);
        sturdy.evade = 20;
        assert_eq!(hit_chance(&weak, &sturdy), -11);

        // Lucky hit: attacker Luck/4 = 3 > roll 2 => always hits even at chance -11.
        weak.luck = 12;
        assert!(resolve_hit(&weak, &sturdy, -11, 2, 100));
        assert!(
            !resolve_hit(&weak, &sturdy, -11, 3, 1),
            "no lucky hit at roll 3"
        );
        // Ordinary threshold: hit when chance >= miss roll.
        assert!(resolve_hit(&enemy, &party, 50, 99, 50));
        assert!(!resolve_hit(&enemy, &party, 50, 99, 51));
        // Lucky dodge: a party attacker (Luck 0 here, so no lucky hit of its own) against an
        // enemy with Luck/4 = 2 > roll 1 always misses, even at chance 255.
        let mut careful = party.clone();
        careful.luck = 0;
        assert!(
            !resolve_hit(&careful, &enemy, 255, 1, 1),
            "lucky dodge forces a miss"
        );
        // ... but not when the roll is too high for the dodge.
        assert!(resolve_hit(&careful, &enemy, 255, 2, 1));
        // The dodge does not apply to enemies attacking the party.
        let mut dodgy_party = party.clone();
        dodgy_party.luck = 99;
        let mut attacker = enemy.clone();
        attacker.luck = 0;
        assert!(resolve_hit(&attacker, &dodgy_party, 255, 1, 1));

        // Critical: ((luck + lvl) - target lvl) >> 2, plus weapon bonus for the party.
        // (12 + 10 - 6) >> 2 = 4; with weapon bonus 3 => 7.
        assert_eq!(critical_chance(&party, &enemy), 4);
        party.crit_bonus = 3;
        assert_eq!(critical_chance(&party, &enemy), 7);
        // Enemies never get the weapon bonus: (8 + 6 - 10) >> 2 = 1 regardless of it.
        enemy.crit_bonus = 50;
        assert_eq!(critical_chance(&enemy, &party), 1);
        // Negative base chances clamp to zero: (0 + 1 - 10) >> 2 = -3.
        let mut low = fighter("L", 1, 1, 0, 0, 0);
        low.side = Side::Enemy;
        assert_eq!(critical_chance(&low, &party), 0);
        // A huge bonus (an always-critical weapon) saturates.
        party.crit_bonus = 255;
        assert!(critical_chance(&party, &enemy) >= 100);
        // Roll mapping: 0 -> 1, 65535 -> 100.
        assert_eq!(percent_roll(0), 1);
        assert_eq!(percent_roll(65_535), 100);
    }

    #[test]
    fn atb_rates_follow_the_documented_formula() {
        // Battle speed 128: 32768 / (120 + 240) = 91, doubled = 182.
        assert_eq!(v_timer_increase(128), 182);
        assert_eq!(v_timer_increase(0), 2 * (32_768 / 120));
        assert_eq!(v_timer_increase(255), 2 * (32_768 / (120 + 255 * 15 / 8)));

        // Party Dex 30 and 40 -> average 35 -> normal speed 85.
        let state = simple_battle(1, zero_start());
        assert_eq!(state.normal_speed(), 85);
        // Party member: (Dex + 50) * 182 / 85.
        assert_eq!(state.gauge_increase(ActorId(0)), Some((40 + 50) * 182 / 85));
        assert_eq!(state.gauge_increase(ActorId(1)), Some((30 + 50) * 182 / 85));
        // Enemy: Dex * 182 / 85 (no +50).
        assert_eq!(state.gauge_increase(ActorId(2)), Some(50 * 182 / 85));
        assert_eq!(state.gauge_increase(ActorId(9)), None);

        // Rounded average: Dex 30 and 41 -> 35.5 -> 36 -> 86.
        let party = vec![fighter("A", 1, 10, 1, 1, 30), fighter("B", 1, 10, 1, 1, 41)];
        let s =
            BattleState::with_config(1, party, vec![fighter("E", 1, 10, 1, 1, 10)], zero_start());
        assert_eq!(s.normal_speed(), 86);
        // An empty party falls back to a normal speed of 50 without dividing by zero.
        let s =
            BattleState::with_config(1, vec![], vec![fighter("E", 1, 10, 1, 1, 10)], zero_start());
        assert_eq!(s.normal_speed(), 50);
    }

    #[test]
    fn ticks_to_fill_for_a_known_dex() {
        // One party member at the average Dex: increase = 182 per tick, so
        // ceil(65535 / 182) = 361 ticks, i.e. just over 6 s at 60 ticks per second.
        let party = vec![fighter("A", 5, 100, 10, 5, 25)];
        let foe = fighter("Slow Foe", 5, 1000, 1, 1, 0); // Dex 0: never fills
        let mut state = BattleState::with_config(7, party, vec![foe], zero_start());
        assert_eq!(state.gauge_increase(ActorId(0)), Some(182));
        assert_eq!(state.gauge_increase(ActorId(1)), Some(0));

        let events = state.step_ticks(360);
        assert!(events.is_empty());
        assert_eq!(state.actor(ActorId(0)).unwrap().gauge, 360 * 182);
        assert!(state.ready_actors().is_empty());
        let events = state.step_ticks(1);
        assert_eq!(events, vec![BattleEvent::GaugeReady { actor: ActorId(0) }]);
        assert_eq!(state.tick(), 361);
        assert_eq!(state.ready_actors(), vec![ActorId(0)]);
        // A full gauge stays full and does not repeat the event.
        assert!(state.step_ticks(50).is_empty());
        assert_eq!(state.actor(ActorId(0)).unwrap().gauge, GAUGE_MAX);

        // In wall-clock terms: 6000 ms is 360 ticks (not ready); 6017 ms reaches 361.
        let party = vec![fighter("A", 5, 100, 10, 5, 25)];
        let foe = fighter("Slow Foe", 5, 1000, 1, 1, 0);
        let mut timed = BattleState::with_config(7, party, vec![foe], zero_start());
        assert!(timed.step(6000).is_empty());
        assert_eq!(timed.tick(), 360);
        let ev = timed.step(17);
        assert_eq!(ev, vec![BattleEvent::GaugeReady { actor: ActorId(0) }]);
    }

    #[test]
    fn step_chunking_does_not_change_the_outcome() {
        let cfg = BattleConfig {
            start: StartMode::Normal,
            ..BattleConfig::default()
        };
        let mut whole = simple_battle(99, cfg);
        let mut chunked = simple_battle(99, cfg);
        let mut chunked_events = Vec::new();
        let whole_events = whole.step(8000);
        for _ in 0..8000 {
            chunked_events.extend(chunked.step(1));
        }
        assert_eq!(whole_events, chunked_events);
        assert_eq!(whole, chunked);
        // Uneven chunks agree too.
        let mut uneven = simple_battle(99, cfg);
        let mut uneven_events = Vec::new();
        for dt in [1u32, 7, 333, 2, 1500, 41, 6116] {
            uneven_events.extend(uneven.step(dt));
        }
        assert_eq!(whole_events, uneven_events);
        assert_eq!(whole, uneven);
        assert!(!whole_events.is_empty(), "something happens in 8 seconds");
    }

    // --- determinism ---

    #[test]
    fn same_seed_same_battle_different_seed_diverges() {
        let cfg = BattleConfig::default();
        let mut a = simple_battle(2024, cfg);
        let mut b = simple_battle(2024, cfg);
        let la = play_out(&mut a, 600);
        let lb = play_out(&mut b, 600);
        assert_eq!(la, lb);
        assert_eq!(a, b);
        assert!(a.outcome().is_some());

        let mut c = simple_battle(2025, cfg);
        let lc = play_out(&mut c, 600);
        assert_ne!(la, lc);
    }

    #[test]
    fn golden_event_log_for_a_fixed_seed() {
        // Regression guard for the RNG draw order and formulas: this exact event sequence
        // must never change silently, because server and clients rely on it.
        let mut state = simple_battle(1234, zero_start());
        let log = play_out(&mut state, 600);
        let rendered: Vec<String> = log
            .iter()
            .map(|e| match e {
                BattleEvent::GaugeReady { actor } => format!("ready {}", actor.0),
                BattleEvent::Damage {
                    actor,
                    target,
                    amount,
                    critical,
                } => {
                    format!(
                        "dmg {}->{} {}{}",
                        actor.0,
                        target.0,
                        amount,
                        if *critical { " crit" } else { "" }
                    )
                }
                BattleEvent::Miss { actor, target } => format!("miss {}->{}", actor.0, target.0),
                BattleEvent::Death { actor } => format!("death {}", actor.0),
                BattleEvent::Victory => "victory".to_string(),
                BattleEvent::Defeat => "defeat".to_string(),
            })
            .collect();
        assert_eq!(state.outcome(), Some(Outcome::Victory));
        assert_eq!(
            rendered.join(" | "),
            "ready 0 | dmg 0->2 90 | ready 1 | dmg 1->2 81 | death 2 | ready 3 | \
             dmg 3->1 28 | ready 0 | dmg 0->3 92 | death 3 | victory"
        );
    }

    // --- command validation ---

    #[test]
    fn command_validation_covers_every_error() {
        let mut state = simple_battle(5, zero_start());
        let attack = |t: u8| Command::Attack { target: ActorId(t) };

        // Gauge not full yet.
        assert_eq!(
            state.submit_command(ActorId(0), attack(2)),
            Err(CommandError::GaugeNotFull(ActorId(0)))
        );
        // Unknown actor, and enemies cannot be commanded.
        assert_eq!(
            state.submit_command(ActorId(77), attack(2)),
            Err(CommandError::UnknownActor(ActorId(77)))
        );
        assert_eq!(
            state.submit_command(ActorId(2), attack(0)),
            Err(CommandError::NotControllable(ActorId(2)))
        );

        // Fill hero A's gauge (the enemies may act in the meantime; keep everyone alive).
        let mut guard = 0;
        while !state.actor(ActorId(0)).unwrap().ready {
            state.step(100);
            guard += 1;
            assert!(guard < 1000, "gauge fills eventually");
        }
        assert_eq!(
            state.submit_command(ActorId(0), attack(77)),
            Err(CommandError::UnknownTarget(ActorId(77)))
        );
        assert_eq!(
            state.submit_command(ActorId(0), attack(1)),
            Err(CommandError::InvalidTarget(ActorId(1))),
            "allies are not valid targets"
        );
        assert_eq!(
            state.submit_command(ActorId(0), attack(0)),
            Err(CommandError::InvalidTarget(ActorId(0))),
            "self is not a valid target"
        );
        // A legal command works and empties the gauge; a second one is refused.
        let events = state.submit_command(ActorId(0), attack(2)).unwrap();
        assert!(events
            .iter()
            .any(|e| matches!(e, BattleEvent::Damage { .. } | BattleEvent::Miss { .. })));
        let actor = state.actor(ActorId(0)).unwrap();
        assert_eq!((actor.gauge, actor.ready), (0, false));
        assert_eq!(
            state.submit_command(ActorId(0), attack(2)),
            Err(CommandError::GaugeNotFull(ActorId(0)))
        );
    }

    #[test]
    fn dead_targets_are_rejected_and_never_act() {
        // The hero cannot miss (hit chance 130) and one-shots the frail enemy (1 HP).
        let hero = fighter("Hero", 20, 500, 200, 50, 60);
        let frail = fighter("Frail", 1, 1, 1, 0, 1);
        let tough = fighter("Tough", 1, 5000, 1, 255, 1);
        let mut state = BattleState::with_config(3, vec![hero], vec![frail, tough], zero_start());
        let mut guard = 0;
        while state.ready_actors().is_empty() {
            state.step(100);
            guard += 1;
            assert!(guard < 1000);
        }
        let ev = state
            .submit_command(ActorId(0), Command::Attack { target: ActorId(1) })
            .unwrap();
        assert!(
            ev.contains(&BattleEvent::Death { actor: ActorId(1) }),
            "{ev:?}"
        );
        assert!(state.outcome().is_none(), "the tough enemy is still alive");

        // The dead enemy is not a valid target and never fills its gauge or acts.
        guard = 0;
        while state.ready_actors().is_empty() {
            state.step(100);
            guard += 1;
            assert!(guard < 2000);
        }
        assert_eq!(
            state.submit_command(ActorId(0), Command::Attack { target: ActorId(1) }),
            Err(CommandError::TargetDead(ActorId(1)))
        );
        let dead = state.actor(ActorId(1)).unwrap();
        assert_eq!((dead.gauge, dead.ready), (0, false));
    }

    #[test]
    fn dead_party_members_cannot_command_and_battle_over_blocks_commands() {
        // A one-member party with 1 HP against a fast, strong enemy dies quickly.
        let hero = fighter("Hero", 1, 1, 1, 0, 1);
        let mut brute = fighter("Brute", 30, 9000, 150, 0, 200);
        brute.luck = 100;
        let mut state = BattleState::with_config(11, vec![hero], vec![brute], zero_start());
        let mut log = Vec::new();
        for _ in 0..2000 {
            log.extend(state.step(100));
            if state.outcome().is_some() {
                break;
            }
        }
        assert_eq!(state.outcome(), Some(Outcome::Defeat));
        assert!(log.contains(&BattleEvent::Death { actor: ActorId(0) }));
        assert_eq!(log.last(), Some(&BattleEvent::Defeat));
        assert_eq!(
            state.submit_command(ActorId(0), Command::Attack { target: ActorId(1) }),
            Err(CommandError::BattleOver)
        );
        // No further events once the outcome is set.
        assert!(state.step(10_000).is_empty());
        assert!(state.step_ticks(10_000).is_empty());
    }

    #[test]
    fn dead_party_members_are_refused_while_the_battle_continues() {
        // Weak (1 HP) and Sturdy fight a fast brute; once the brute happens to pick Weak,
        // Weak is dead but the battle goes on because Sturdy survives.
        let weak = fighter("Weak", 1, 1, 1, 0, 1);
        let sturdy = fighter("Sturdy", 20, 1_000_000, 10, 255, 1);
        let brute = fighter("Brute", 30, 9000, 150, 0, 200);
        let mut state = BattleState::with_config(21, vec![weak, sturdy], vec![brute], zero_start());
        let mut guard = 0;
        while state.actor(ActorId(0)).is_some_and(Combatant::is_alive) {
            state.step(100);
            guard += 1;
            assert!(guard < 5000, "the brute eventually picks the weak hero");
        }
        assert!(state.outcome().is_none());
        assert_eq!(
            state.submit_command(ActorId(0), Command::Attack { target: ActorId(2) }),
            Err(CommandError::ActorDead(ActorId(0)))
        );
        // The dead hero is not listed as ready and its gauge is cleared.
        assert!(!state.ready_actors().contains(&ActorId(0)));
        assert_eq!(state.actor(ActorId(0)).unwrap().gauge, 0);
    }

    // --- whole battles ---

    #[test]
    fn victory_run_emits_victory_once_and_totals_rewards() {
        let mut state = simple_battle(31337, zero_start());
        let log = play_out(&mut state, 3000);
        assert_eq!(state.outcome(), Some(Outcome::Victory));
        assert_eq!(
            log.iter().filter(|e| **e == BattleEvent::Victory).count(),
            1
        );
        assert_eq!(log.last(), Some(&BattleEvent::Victory));
        assert!(!log.contains(&BattleEvent::Defeat));
        // Both enemies died exactly once.
        for id in [2u8, 3] {
            assert_eq!(
                log.iter()
                    .filter(|e| **e == BattleEvent::Death { actor: ActorId(id) })
                    .count(),
                1
            );
        }
        assert_eq!(
            state.rewards(),
            Reward {
                exp: 32,
                gil: 48,
                ap: 3
            }
        );
        // Nothing happens afterwards.
        assert!(state.step(5000).is_empty());
    }

    #[test]
    fn defeat_when_the_party_never_acts() {
        let mut state = simple_battle(8, zero_start());
        let mut log = Vec::new();
        for _ in 0..20_000 {
            log.extend(state.step(100));
            if state.outcome().is_some() {
                break;
            }
        }
        assert_eq!(state.outcome(), Some(Outcome::Defeat));
        assert_eq!(log.iter().filter(|e| **e == BattleEvent::Defeat).count(), 1);
        assert_eq!(log.last(), Some(&BattleEvent::Defeat));
        assert!(!log.contains(&BattleEvent::Victory));
        assert!(
            state.ready_actors().is_empty(),
            "dead heroes are not 'ready'"
        );
        assert_eq!(state.rewards(), Reward::default());
    }

    #[test]
    fn hits_misses_and_crits_all_occur_across_seeds() {
        let mut hits = 0;
        let mut misses = 0;
        let mut crits = 0;
        for seed in 0..300u64 {
            let mut hero = fighter("Hero", 15, 100_000, 40, 10, 30);
            hero.accuracy = 60; // a real miss chance
            hero.luck = 60;
            hero.crit_bonus = 20;
            let mut foe = fighter("Foe", 15, 1_000_000, 10, 10, 1);
            foe.evade = 20;
            let mut state = BattleState::with_config(seed, vec![hero], vec![foe], zero_start());
            // Wait until ready, then attack once.
            while state.ready_actors().is_empty() {
                state.step(100);
            }
            for e in state
                .submit_command(ActorId(0), Command::Attack { target: ActorId(1) })
                .unwrap()
            {
                match e {
                    BattleEvent::Damage { critical, .. } => {
                        hits += 1;
                        if critical {
                            crits += 1;
                        }
                    }
                    BattleEvent::Miss { .. } => misses += 1,
                    _ => {}
                }
            }
        }
        assert_eq!(hits + misses, 300);
        assert!(hits > 100 && misses > 10, "hits {hits}, misses {misses}");
        assert!(crits > 10 && crits < hits, "crits {crits} of {hits}");
    }

    #[test]
    fn back_row_halves_short_range_damage_only() {
        // Same seed and same draw order in every run, so the only difference between the
        // runs is the row rule. Hit chance is 114 (cannot miss).
        let fixed = |row: Row, short_range: bool, target_row: Row| {
            let mut hero = fighter("Hero", 20, 100, 50, 0, 30);
            hero.row = row;
            hero.short_range = short_range;
            hero.luck = 100;
            let mut foe = fighter("Foe", 20, 1_000_000, 10, 10, 1);
            foe.row = target_row;
            let mut state = BattleState::with_config(4, vec![hero], vec![foe], zero_start());
            while state.ready_actors().is_empty() {
                state.step(100);
            }
            let ev = state
                .submit_command(ActorId(0), Command::Attack { target: ActorId(1) })
                .unwrap();
            ev.iter()
                .find_map(|e| match e {
                    BattleEvent::Damage { amount, .. } => Some(*amount),
                    _ => None,
                })
                .expect("a hit")
        };
        let front = fixed(Row::Front, true, Row::Front);
        let back_short = fixed(Row::Back, true, Row::Front);
        let target_back_short = fixed(Row::Front, true, Row::Back);
        let back_long = fixed(Row::Back, false, Row::Back);
        assert!(
            back_short < front,
            "short range from the back row is halved"
        );
        assert_eq!(
            back_short, target_back_short,
            "a back-row target halves it too"
        );
        assert_eq!(back_long, front, "long range ignores rows");
    }

    // --- setup edge cases ---

    #[test]
    fn empty_sides_resolve_immediately_and_deliver_the_event_once() {
        let hero = fighter("Hero", 5, 50, 10, 5, 20);
        let foe = fighter("Foe", 5, 50, 10, 5, 20);
        let mut won = BattleState::new(1, vec![hero.clone()], vec![]);
        assert_eq!(won.outcome(), Some(Outcome::Victory));
        assert_eq!(won.step(1000), vec![BattleEvent::Victory]);
        assert!(won.step(1000).is_empty());

        let mut lost = BattleState::new(1, vec![], vec![foe.clone()]);
        assert_eq!(lost.outcome(), Some(Outcome::Defeat));
        assert_eq!(lost.step_ticks(1), vec![BattleEvent::Defeat]);

        // An enemy that starts with zero HP is dead from the start and never acts.
        let mut corpse = foe;
        corpse.hp = 0;
        let state = BattleState::new(1, vec![hero], vec![corpse]);
        assert_eq!(state.outcome(), Some(Outcome::Victory));
    }

    #[test]
    fn start_modes_set_initial_gauges() {
        let mk = |start: StartMode| {
            let party = vec![
                fighter("A", 5, 50, 10, 5, 30),
                fighter("B", 5, 50, 10, 5, 30),
            ];
            let enemies = vec![fighter("E", 5, 50, 10, 5, 30)];
            BattleState::with_config(
                77,
                party,
                enemies,
                BattleConfig {
                    start,
                    ..BattleConfig::default()
                },
            )
        };
        // Zero: everything empty.
        let s = mk(StartMode::Zero);
        assert!(s.actors().iter().all(|c| c.gauge == 0 && !c.ready));
        // Normal: partial fills only, never full.
        let s = mk(StartMode::Normal);
        assert!(s
            .actors()
            .iter()
            .all(|c| c.gauge <= INITIAL_GAUGE_MAX && !c.ready));
        assert!(s.actors().iter().any(|c| c.gauge > 0));

        // Preemptive: party full and waiting, enemies empty; events delivered with the first
        // step (or command).
        let mut s = mk(StartMode::Preemptive);
        assert_eq!(s.ready_actors(), vec![ActorId(0), ActorId(1)]);
        assert_eq!(s.actor(ActorId(2)).unwrap().gauge, 0);
        let first = s.step(0);
        assert_eq!(
            first,
            vec![
                BattleEvent::GaugeReady { actor: ActorId(0) },
                BattleEvent::GaugeReady { actor: ActorId(1) }
            ]
        );
        assert!(s.step(0).is_empty());

        // Back attack: the enemy is full, so it strikes on the very first tick.
        let mut s = mk(StartMode::BackAttack);
        assert!(s.ready_actors().is_empty());
        let ev = s.step_ticks(1);
        assert_eq!(ev[0], BattleEvent::GaugeReady { actor: ActorId(2) });
        assert!(ev.iter().any(|e| matches!(
            e,
            BattleEvent::Damage {
                actor: ActorId(2),
                ..
            } | BattleEvent::Miss {
                actor: ActorId(2),
                ..
            }
        )));

        // Pending start events are also delivered by an early command.
        let mut s = mk(StartMode::Preemptive);
        let ev = s
            .submit_command(ActorId(0), Command::Attack { target: ActorId(2) })
            .unwrap();
        assert_eq!(ev[0], BattleEvent::GaugeReady { actor: ActorId(0) });

        assert_eq!(
            StartMode::from_layout(BattleLayout::Preemptive),
            StartMode::Preemptive
        );
        assert_eq!(
            StartMode::from_layout(BattleLayout::BackAttack),
            StartMode::BackAttack
        );
        assert_eq!(
            StartMode::from_layout(BattleLayout::Pincer),
            StartMode::Normal
        );
    }

    #[test]
    fn construction_sanitises_inputs() {
        // Oversized sides are truncated; hp above max is clamped; sides are reassigned.
        let party: Vec<Combatant> = (0..40)
            .map(|i| fighter(&format!("P{i}"), 5, 50, 5, 5, 5))
            .collect();
        let mut odd = fighter("Odd", 5, 50, 5, 5, 5);
        odd.hp = 9999;
        odd.side = Side::Enemy;
        let state = BattleState::new(1, party, vec![odd]);
        assert_eq!(state.party_ids().len(), MAX_PER_SIDE);
        assert_eq!(state.enemy_ids().len(), 1);
        let enemy = state.actor(state.enemy_ids()[0]).unwrap();
        assert_eq!((enemy.hp, enemy.side), (50, Side::Enemy));
        assert!(state
            .actors()
            .iter()
            .take(MAX_PER_SIDE)
            .all(|c| c.side == Side::Party));

        // Config clamps: tick rate in 1..=1000.
        let cfg = BattleConfig {
            ticks_per_second: 0,
            ..BattleConfig::default()
        };
        let s = BattleState::with_config(1, vec![], vec![], cfg);
        assert_eq!(s.config().ticks_per_second, 1);
        let cfg = BattleConfig {
            ticks_per_second: 100_000,
            ..BattleConfig::default()
        };
        let s = BattleState::with_config(1, vec![], vec![], cfg);
        assert_eq!(s.config().ticks_per_second, 1000);

        // Absurd step sizes are bounded: one call consumes at most MAX_STEP_MS of time
        // (60 s at the default 60 ticks per second = 3600 ticks) or MAX_STEP_TICKS ticks.
        let mut s = simple_battle(1, zero_start());
        let _ = s.step(u32::MAX);
        assert!(s.tick() <= 3600);
        let before = s.tick();
        let _ = s.step_ticks(u64::MAX);
        assert!(s.tick() - before <= MAX_STEP_TICKS);
    }

    #[test]
    fn absurd_stat_values_do_not_overflow_or_panic() {
        let max = DamageInput {
            attack: u32::MAX,
            level: u32::MAX,
            power: u32::MAX,
            defense: u32::MAX,
            critical: true,
            halve_for_row: false,
            variance_roll: u32::MAX,
        };
        assert!((1..=MAX_DAMAGE).contains(&physical_damage(&max)));

        let mut a = Combatant::new("A", 255, u32::MAX);
        let mut b = Combatant::new("B", 255, u32::MAX);
        for c in [&mut a, &mut b] {
            c.attack = u32::MAX;
            c.defense = u32::MAX;
            c.dex = u32::MAX;
            c.luck = u32::MAX;
            c.evade = u32::MAX;
            c.accuracy = u32::MAX;
            c.attack_power = u32::MAX;
            c.crit_bonus = u32::MAX;
            c.max_mp = u32::MAX;
        }
        let mut state = BattleState::new(1, vec![a.clone()], vec![b.clone()]);
        assert_eq!(state.normal_speed(), u32::MAX); // saturated, no overflow
        let _ = state.step(60_000);
        let _ = hit_chance(&a, &b);
        let _ = critical_chance(&a, &b);
        let _ = turn_timer_increase(Side::Party, u32::MAX, u32::MAX, 0);
        // The engine stays usable.
        for id in state.ready_actors() {
            let target = state.enemy_ids()[0];
            let _ = state.submit_command(id, Command::Attack { target });
        }
    }

    // --- data-driven constructors ---

    #[test]
    fn combatants_from_scene_data() {
        let bin = SceneBin::parse(&pack_scene_bin(&[gzip(&demo_scene(0).raw)], 4)).unwrap();
        let scene = bin.scene(0).unwrap();
        let enemies = enemies_for_formation(scene, 0);
        assert_eq!(enemies.len(), 2);
        let grunt = &enemies[0];
        assert_eq!(grunt.name, "Test Grunt");
        assert_eq!(grunt.side, Side::Enemy);
        assert_eq!((grunt.level, grunt.hp, grunt.max_hp), (7, 316, 316));
        assert_eq!(
            (
                grunt.attack,
                grunt.defense,
                grunt.dex,
                grunt.luck,
                grunt.evade
            ),
            (21, 11, 60, 5, 3)
        );
        assert_eq!((grunt.mp, grunt.max_mp), (12, 12));
        // The grunt's basic attack is "Test Bite" (accuracy 95, power 20), not the magic one.
        assert_eq!((grunt.accuracy, grunt.attack_power), (95, 20));
        assert!(grunt.short_range);
        assert_eq!(
            grunt.reward,
            Reward {
                exp: 77,
                gil: 55,
                ap: 4
            }
        );
        let hound = &enemies[1];
        assert_eq!((hound.accuracy, hound.attack_power), (90, 16));
        // Formation 1 has two grunts; empty and out-of-range formations give nothing.
        assert_eq!(enemies_for_formation(scene, 1).len(), 2);
        assert!(enemies_for_formation(scene, 2).is_empty());
        assert!(enemies_for_formation(scene, 9).is_empty());

        // Without a basic attack the enemy falls back to Power 16 at 100%.
        let rec = scene.enemies[0].as_ref().unwrap();
        let fallback = Combatant::from_enemy(rec, None);
        assert_eq!(
            (fallback.accuracy, fallback.attack_power),
            (100, ATTACK_COMMAND_POWER)
        );
    }

    #[test]
    fn combatants_from_kernel_data() {
        let kernel = Kernel::parse(&synthetic_kernel_bytes()).unwrap();
        // Cloud (init id 0): level 6; primary stats 16 each; starting weapon 0 (attack 10,
        // accuracy 90, short range) and armor 0 (defense 5, evade 0).
        let cloud = Combatant::from_character(&kernel, CharacterId::Cloud, 6, None, None).unwrap();
        assert_eq!(cloud.name, "Cloud");
        assert_eq!(cloud.side, Side::Party);
        assert_eq!(cloud.level, 6);
        assert_eq!((cloud.hp, cloud.max_hp), (280, 280));
        assert_eq!((cloud.mp, cloud.max_mp), (50, 50));
        assert_eq!(cloud.attack, 16 + 10);
        assert_eq!(cloud.defense, 16 + 5);
        assert_eq!((cloud.dex, cloud.luck, cloud.evade), (16, 16, 0));
        assert_eq!(
            (cloud.accuracy, cloud.crit_bonus, cloud.attack_power),
            (90, 0, 16)
        );
        assert!(cloud.short_range);
        assert_eq!(cloud.row, Row::Front);

        // Explicit equipment overrides the starting kit: weapon 1 is long range with crit 1.
        let gunner =
            Combatant::from_character(&kernel, CharacterId::Cloud, 6, Some(1), Some(2)).unwrap();
        // Armor 2: defense 5 + 3*2 = 11, evade 2*2 = 4 percent.
        assert_eq!(gunner.attack, 16 + 11);
        assert_eq!(gunner.defense, 16 + 11);
        assert_eq!(gunner.evade, 4);
        assert_eq!((gunner.accuracy, gunner.crit_bonus), (91, 1));
        assert!(!gunner.short_range);

        // Aerith starts in the back row (init record id 3).
        let aerith =
            Combatant::from_character(&kernel, CharacterId::Aeris, 10, None, None).unwrap();
        assert_eq!(aerith.row, Row::Back);
        // Cait Sith has no initialization record: unarmed, unarmored, 100% accuracy.
        let cait =
            Combatant::from_character(&kernel, CharacterId::CaitSith, 10, None, None).unwrap();
        let stats = kernel.growth.stat_block(CharacterId::CaitSith, 10).unwrap();
        assert_eq!(cait.attack, u32::from(stats.strength));
        assert_eq!(cait.defense, u32::from(stats.vitality));
        assert_eq!((cait.accuracy, cait.evade, cait.crit_bonus), (100, 0, 0));
        assert!(cait.short_range);

        // Levels are clamped by the stat block.
        let capped =
            Combatant::from_character(&kernel, CharacterId::Cloud, 255, None, None).unwrap();
        assert_eq!(capped.level, 99);
    }

    #[test]
    fn shared_battle_party_of_two_players_vs_a_formation() {
        // End-to-end with synthetic data: two player characters against a scene formation.
        let kernel = Kernel::parse(&synthetic_kernel_bytes()).unwrap();
        let bin = SceneBin::parse(&pack_scene_bin(&[gzip(&demo_scene(0).raw)], 4)).unwrap();
        let party = vec![
            Combatant::from_character(&kernel, CharacterId::Cloud, 12, None, None).unwrap(),
            Combatant::from_character(&kernel, CharacterId::Barret, 12, None, None).unwrap(),
        ];
        let enemies = enemies_for_formation(bin.scene(0).unwrap(), 0);
        let mut state = BattleState::new(0xFEED_FACE, party, enemies);
        let log = play_out(&mut state, 5000);
        assert!(state.outcome().is_some());
        assert!(log.iter().any(|e| matches!(e, BattleEvent::Damage { .. })));
        // Replaying with the same seed reproduces the identical log.
        let party = vec![
            Combatant::from_character(&kernel, CharacterId::Cloud, 12, None, None).unwrap(),
            Combatant::from_character(&kernel, CharacterId::Barret, 12, None, None).unwrap(),
        ];
        let enemies = enemies_for_formation(bin.scene(0).unwrap(), 0);
        let mut replay = BattleState::new(0xFEED_FACE, party, enemies);
        assert_eq!(play_out(&mut replay, 5000), log);
    }

    // --- serde ---

    #[test]
    fn events_and_commands_roundtrip_through_json() {
        let events = vec![
            BattleEvent::GaugeReady { actor: ActorId(1) },
            BattleEvent::Damage {
                actor: ActorId(0),
                target: ActorId(3),
                amount: 42,
                critical: true,
            },
            BattleEvent::Miss {
                actor: ActorId(2),
                target: ActorId(0),
            },
            BattleEvent::Death { actor: ActorId(3) },
            BattleEvent::Victory,
            BattleEvent::Defeat,
        ];
        let json = serde_json::to_string(&events).unwrap();
        let back: Vec<BattleEvent> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, events);

        let cmd = Command::Attack { target: ActorId(4) };
        let back: Command = serde_json::from_str(&serde_json::to_string(&cmd).unwrap()).unwrap();
        assert_eq!(back, cmd);
        let out: Outcome =
            serde_json::from_str(&serde_json::to_string(&Outcome::Victory).unwrap()).unwrap();
        assert_eq!(out, Outcome::Victory);
    }
}
