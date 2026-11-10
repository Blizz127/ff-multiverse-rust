//! The first Mako reactor's fights against the owner's real files, pinned in NUMBERS (not bytes): the enemies the
//! formations of the reactor's fields send, the party that meets them, how the turn timers start and fill, and the
//! damage, hit and critical rates of every blow between them, checked against a small reference implementation of
//! the disassembly-derived pseudocode of <https://wiki.ffrtt.ru/index.php/FF7/DamageFormula>. Every test is
//! `#[ignore]`d and reads the data directory from `FMV_FF7_DATA` (the game's `data/` folder); without it they print a
//! note and pass.
//!
//! ```text
//! FMV_FF7_DATA=/path/to/ff7/data cargo test -p fmv-ff7-battle --test md1_real -- --ignored --nocapture
//! ```
//!
//! The files are only read. The numbers asserted here are the game's own (stats and counts), never its bytes.

use std::path::PathBuf;

use fmv_ff7_battle::kernel::{CharacterId, Kernel};
use fmv_ff7_battle::rules::{
    action_damage, critical_chance, enemies_for_formation, hit_chance, hit_chance_with, is_critical, resolve_hit, restore_amount,
    ActorId, BattleConfig, BattleEvent, BattleState, Combatant, Command, Elemental, Outcome, Rolls, Row, Side, StartMode,
    ATTACK_COMMAND_POWER, BACK_ATTACK_START_PEAK, GAUGE_MAX, INITIAL_GAUGE_MAX, NORMAL_START_PEAK, PREEMPTIVE_ENEMY_DIVISOR,
};
use fmv_ff7_battle::scene::SceneBin;

fn load() -> Option<(SceneBin, Kernel)> {
    let Some(root) = std::env::var_os("FMV_FF7_DATA") else {
        eprintln!("FMV_FF7_DATA is not set; skipping real-data test");
        return None;
    };
    let root = PathBuf::from(root);
    let find = |candidates: &[&str]| {
        candidates.iter().map(|rel| root.join(rel)).find(|p| p.is_file()).unwrap_or_else(|| panic!("{} has none of {candidates:?}", root.display()))
    };
    let scene = find(&["lang-en/battle/scene.bin", "battle/scene.bin", "battle/SCENE.BIN"]);
    let kernel = find(&["lang-en/kernel/kernel.bin", "kernel/kernel.bin", "kernel/KERNEL.BIN"]);
    Some((SceneBin::open(scene).expect("scene.bin parses"), Kernel::open(kernel).expect("kernel.bin parses")))
}

/// The formations the first reactor's fields send (the encounter tables of `md1stin`/`md1_1`/`md1_2`: 301-303; `nrthmk`:
/// 305-307; `nmkin_1`/`nmkin_2`: 308-313; `nmkin_3`: 316-318; `nmkin_4`: 320-322, and the Guard Scorpion's story fight, 324) and the
/// enemies in each, in placement order. The tables themselves are checked by `fmv-ff7`'s and `fmv-server`'s real-data tests.
const MD1: &[(u16, &[&str])] = &[
    (301, &["MP", "MP"]),
    (302, &["Guard Hound"]),
    (303, &["MP", "Guard Hound"]),
    (305, &["Mono Drive", "Mono Drive"]),
    (306, &["MP", "MP", "Mono Drive"]),
    (307, &["MP", "MP"]),
    (308, &["Mono Drive", "Mono Drive", "1st Ray"]),
    (309, &["1st Ray", "1st Ray"]),
    (310, &["Grunt", "Grunt"]),
    (311, &["1st Ray", "Grunt", "1st Ray"]),
    (312, &["1st Ray", "1st Ray", "1st Ray"]),
    (313, &["Grunt", "Grunt", "Grunt"]),
    (316, &["Grunt", "Grunt", "Mono Drive", "Mono Drive"]),
    (317, &["Grunt", "Grunt", "Mono Drive", "Mono Drive", "Mono Drive"]),
    (318, &["Grunt", "Grunt", "Grunt"]),
    (320, &["Sweeper"]),
    (321, &["Sweeper", "Grunt", "Grunt"]),
    (322, &["Grunt", "Grunt", "Sweeper"]),
    (324, &["Guard Scorpion"]),
];

/// One enemy's record: level, HP, speed (its Dexterity), Strength, Defense, Magic, Magic defense, Luck, Evade.
type Record = (u8, u32, u8, u8, u8, u8, u8, u8, u8);

/// The records of the first reactor's enemies (`scene.bin`, offsets 0x20-0x27 and 0xA4 of the enemy data, see
/// <https://wiki.ffrtt.ru/index.php/FF7/Battle/Battle_Scenes>), which are also the numbers the bestiaries print.
const RECORDS: &[(&str, Record)] = &[
    ("MP", (2, 30, 50, 6, 2, 0, 0, 4, 0)),
    ("Guard Hound", (3, 42, 64, 8, 2, 2, 1, 6, 6)),
    ("Mono Drive", (2, 28, 49, 3, 3, 3, 2, 2, 6)),
    ("1st Ray", (4, 18, 40, 10, 1, 0, 0, 0, 0)),
    ("Grunt", (7, 40, 58, 12, 5, 2, 1, 8, 4)),
    ("Sweeper", (8, 140, 48, 18, 10, 0, 2, 1, 0)),
    ("Guard Scorpion", (12, 800, 60, 30, 20, 15, 150, 1, 0)),
];

fn record_of(name: &str) -> Record {
    RECORDS.iter().find(|(n, _)| *n == name).map(|(_, r)| *r).unwrap_or_else(|| panic!("no record for {name}"))
}

fn cloud(kernel: &Kernel, level: u8) -> Combatant {
    Combatant::from_character(kernel, CharacterId::Cloud, level, None, None).expect("Cloud")
}

fn barret(kernel: &Kernel, level: u8) -> Combatant {
    Combatant::from_character(kernel, CharacterId::Barret, level, None, None).expect("Barret")
}

/// The first reactor's fighters at level 6, the level the game starts them at: the canon pair and a player on Barret's curve.
fn party(kernel: &Kernel) -> Vec<Combatant> {
    vec![cloud(kernel, 6), barret(kernel, 6), barret(kernel, 6)]
}

fn enemies_of(bin: &SceneBin, formation: u16) -> Vec<Combatant> {
    let f = bin.formation(usize::from(formation)).expect("a formation");
    enemies_for_formation(f.scene, f.slot)
}

// ---------------------------------------------------------------------------------------------------------------
// 1. The enemies and the party
// ---------------------------------------------------------------------------------------------------------------

#[test]
#[ignore = "needs FMV_FF7_DATA pointing at the owner's FF7 data directory"]
fn the_first_reactors_formations_hold_the_enemies_and_stats_the_game_gives_them() {
    let Some((bin, _)) = load() else { return };
    for &(formation, names) in MD1 {
        let f = bin.formation(usize::from(formation)).expect("a formation");
        let enemies = enemies_of(&bin, formation);
        assert_eq!(enemies.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), names, "formation {formation}");
        // Every one of them is a normal fight that can be run from, except the boss.
        assert_eq!(StartMode::from_layout(f.setup().layout), StartMode::Normal, "formation {formation}");
        assert_eq!(f.setup().run_difficulty().is_some(), formation != 324, "formation {formation}");
        for e in &enemies {
            let (level, hp, dex, strength, defense, magic, magic_defense, luck, evade) = record_of(&e.name);
            assert_eq!(
                (e.level, e.max_hp, e.dex, e.attack, e.defense, e.magic, e.magic_defense, e.luck, e.evade),
                (level, hp, u32::from(dex), u32::from(strength), u32::from(defense), u32::from(magic), u32::from(magic_defense), u32::from(luck), u32::from(evade)),
                "formation {formation}: {}",
                e.name
            );
            assert_eq!(e.hp, e.max_hp);
            assert!(e.ai.is_some(), "formation {formation}: {} has its own scripts", e.name);
        }
    }
}

#[test]
#[ignore = "needs FMV_FF7_DATA pointing at the owner's FF7 data directory"]
fn the_canon_pair_at_level_six_has_these_numbers() {
    let Some((_, kernel)) = load() else { return };
    let (c, b) = (cloud(&kernel, 6), barret(&kernel, 6));
    // (max HP, max MP, attack = Strength + weapon, defense, Magic, magic defense, Dexterity, Luck, weapon accuracy, short range)
    assert_eq!((c.max_hp, c.max_mp, c.attack, c.defense, c.magic, c.magic_defense, c.dex, c.luck, c.accuracy, c.short_range), (314, 50, 38, 24, 19, 17, 10, 16, 96, true));
    assert_eq!((b.max_hp, b.max_mp, b.attack, b.defense, b.magic, b.magic_defense, b.dex, b.luck, b.accuracy, b.short_range), (332, 44, 33, 27, 16, 14, 8, 14, 97, false));
    assert_eq!((c.evade, c.magic_evade, c.crit_bonus, c.row), (0, 0, 0, Row::Front));
    assert_eq!(c.attack_power, ATTACK_COMMAND_POWER);
    // They fight with Cure, Fire, Ice and Bolt at once: Cure never misses, the three bolts have a hit rate of 100.
    let spells: Vec<(u16, u32)> = c.spells.iter().map(|s| (s.index, s.spec.accuracy)).collect();
    assert_eq!(spells, [(0, 255), (27, 100), (30, 100), (33, 100)]);
}

// ---------------------------------------------------------------------------------------------------------------
// 2. The turn timers
// ---------------------------------------------------------------------------------------------------------------

/// Ticks a gauge at `gauge` needs to be full at `increase` per tick.
fn ticks_to_fill(gauge: u32, increase: u32) -> Option<u64> {
    (increase > 0).then(|| u64::from(GAUGE_MAX - gauge.min(GAUGE_MAX)).div_ceil(u64::from(increase)))
}

#[test]
#[ignore = "needs FMV_FF7_DATA pointing at the owner's FF7 data directory"]
fn gauges_fill_at_the_documented_rates_and_the_battles_turns_come_in_the_order_they_fill() {
    let Some((bin, kernel)) = load() else { return };
    // The FF7 Project's formulas at the default battle speed: V-timer 2 * (32768 / (120 + 128 * 15 / 8)) = 182; the party's
    // average Dexterity (10, 8, 8) rounds to 9, so the normal speed is 59 and a member gains (Dex + 50) * 182 / 59 a tick,
    // an enemy Dex * 182 / 59.
    let party_increase = [(10 + 50) * 182 / 59, (8 + 50) * 182 / 59, (8 + 50) * 182 / 59];
    assert_eq!(party_increase, [185, 178, 178]);
    let enemy_increase = |dex: u32| dex * 182 / 59;
    // (enemy, its gain a tick, ticks to fill from empty, seconds at the default 60 ticks a second)
    for (name, per_tick, ticks) in [("MP", 154, 426), ("Guard Hound", 197, 333), ("Mono Drive", 151, 435), ("1st Ray", 123, 533), ("Grunt", 178, 369), ("Sweeper", 148, 443)] {
        let dex = u32::from(record_of(name).2);
        assert_eq!(enemy_increase(dex), per_tick, "{name}");
        assert_eq!(ticks_to_fill(0, per_tick), Some(ticks), "{name}");
    }
    // The party's own: 355 and 369 ticks (5.9 s and 6.2 s) from empty.
    assert_eq!(party_increase.map(|inc| ticks_to_fill(0, inc)), [Some(355), Some(369), Some(369)]);

    let mut checked = 0usize;
    for &(formation, _) in MD1 {
        for seed in 0..25u64 {
            let foes = enemies_of(&bin, formation);
            let mut state = BattleState::with_config(seed, party(&kernel), foes, BattleConfig::default());
            assert_eq!(state.normal_speed(), 59);
            let n = state.actors().len();
            let start: Vec<u32> = state.actors().iter().map(|c| c.gauge).collect();
            // Fergusson's "Starting Turn Timers", normal battle: nobody is under 37.5% or full, the luckiest is at 87.5%.
            assert_eq!(start.iter().copied().max(), Some(NORMAL_START_PEAK), "formation {formation}, seed {seed}: {start:?}");
            assert!(start.iter().all(|&g| g >= NORMAL_START_PEAK - INITIAL_GAUGE_MAX), "formation {formation}, seed {seed}: {start:?}");
            let increase: Vec<u32> = (0..n).map(|i| state.gauge_increase(ActorId(i as u8)).expect("an actor")).collect();
            for (i, &inc) in increase.iter().enumerate().take(3) {
                assert_eq!(inc, party_increase[i], "the party member {i}");
            }
            for (i, c) in state.actors().iter().enumerate().skip(3) {
                assert_eq!(increase[i], enemy_increase(c.dex), "enemy {i}: {}", c.name);
            }
            // When each one's gauge first fills, from its start and its gain; nobody is ever commanded here, so the party
            // waits full and an enemy acts the tick it fills (and starts again).
            let predicted: Vec<Option<u64>> = (0..n).map(|i| ticks_to_fill(start[i], increase[i])).collect();
            let horizon = predicted.iter().flatten().max().copied().unwrap_or(0) + 1;
            let mut first_ready: Vec<Option<u64>> = vec![None; n];
            let mut order: Vec<(u64, u8)> = Vec::new();
            for _ in 0..horizon {
                for e in state.step_ticks(1) {
                    if let BattleEvent::GaugeReady { actor } = e {
                        let slot = &mut first_ready[usize::from(actor.0)];
                        if slot.is_none() {
                            *slot = Some(state.tick());
                            order.push((state.tick(), actor.0));
                        }
                    }
                }
            }
            assert_eq!(first_ready, predicted, "formation {formation}, seed {seed}: the tick each gauge first fills");
            // The turns come in the order the gauges fill, the party before the enemies on a tick, then by position.
            let mut sorted = order.clone();
            sorted.sort_unstable();
            assert_eq!(order, sorted, "formation {formation}, seed {seed}");
            checked += 1;
        }
    }
    assert_eq!(checked, MD1.len() * 25);
}

#[test]
#[ignore = "needs FMV_FF7_DATA pointing at the owner's FF7 data directory"]
fn the_special_arrivals_start_the_timers_the_way_the_faq_says() {
    let Some((bin, kernel)) = load() else { return };
    for seed in 0..40u64 {
        let foes = || enemies_of(&bin, 313);
        // A pre-emptive strike (and a side attack): the party full and ready at once, each enemy's draw over 8.
        let config = |start| BattleConfig { start, ..BattleConfig::default() };
        let pre = BattleState::with_config(seed, party(&kernel), foes(), config(StartMode::Preemptive));
        assert_eq!(pre.ready_actors(), [ActorId(0), ActorId(1), ActorId(2)]);
        for c in pre.actors().iter().filter(|c| c.side == Side::Enemy) {
            assert!(c.gauge <= INITIAL_GAUGE_MAX / PREEMPTIVE_ENEMY_DIVISOR, "seed {seed}: an enemy starts at most 6.25% full");
        }
        // A back attack (and an ambush): the party empty, the enemies between 43.75% and 93.75%.
        let back = BattleState::with_config(seed, party(&kernel), foes(), config(StartMode::BackAttack));
        assert!(back.ready_actors().is_empty());
        for c in back.actors() {
            match c.side {
                Side::Party => assert_eq!(c.gauge, 0),
                Side::Enemy => assert!((BACK_ATTACK_START_PEAK - INITIAL_GAUGE_MAX..=BACK_ATTACK_START_PEAK).contains(&c.gauge), "seed {seed}: {}", c.gauge),
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// 3. The formulas, against a reference implementation of the pseudocode
// ---------------------------------------------------------------------------------------------------------------

/// FF7/DamageFormula, "X1": `x = (Level * Attack) / 32; y = (Level + Attack) / 32; z = Attack + x * y;
/// z = z * (512 - Defense) * Power; Damage = z >> 13`.
fn ref_x1(attack: u64, level: u64, defense: u64, power: u64) -> u64 {
    let (x, y) = (level * attack / 32, (level + attack) / 32);
    ((attack + x * y) * (512 - defense) * power) >> 13
}

/// "X2": `Base = (Attack + Level) * 6; Damage = (512 - Defense) * Base * Power / 8192`.
fn ref_x2(attack: u64, level: u64, defense: u64, power: u64) -> u64 {
    ((512 - defense) * ((attack + level) * 6) * power) >> 13
}

/// "Variance": `Damage = Damage * ([0..255] + 3841) >> 12`, a result of 0 raised to 1.
fn ref_variance(damage: u64, roll: u64) -> u64 {
    ((damage * (roll + 3841)) >> 12).max(1)
}

/// The physical hit routine (0x5DDBB0): `Physical Chance = (Actor's Dex >> 2) + hit rate`; an enemy's evade is its stat and a
/// party member's `(Dex >> 2) + evade`; `HitChance = Physical Chance + Actor's evade - Target's evade`; 0 becomes 1.
fn ref_hit_chance(actor: &Combatant, target: &Combatant, hit_rate: i64) -> i64 {
    let evade = |c: &Combatant| match c.side {
        Side::Enemy => i64::from(c.evade),
        Side::Party => i64::from(c.dex >> 2) + i64::from(c.evade),
    };
    let chance = i64::from(actor.dex >> 2) + hit_rate + evade(actor) - evade(target);
    if chance == 0 {
        1
    } else {
        chance
    }
}

/// The luck lines and the miss test of the same routine: a lucky actor (`Luck / 4 > roll`) connects, else a party member
/// against an enemy whose `Luck / 4 > roll` misses; otherwise it misses when `HitChance < Miss Chance`.
fn ref_hits(actor: &Combatant, target: &Combatant, chance: i64, luck_roll: u32, miss_roll: u32) -> bool {
    let mut chance = chance;
    if actor.luck / 4 > luck_roll {
        chance = 255;
    } else if actor.side == Side::Party && target.side == Side::Enemy && target.luck / 4 > luck_roll {
        chance = 0;
    }
    !(chance < i64::from(miss_roll))
}

/// FF7's `rand(0..=65535) * 99 / 65535 + 1`.
fn roll_of(raw: u32) -> u32 {
    raw * 99 / 65_535 + 1
}

/// The critical routine (0x5DDF8E): `((Luck + Level) - Target's level) >> 2` (a party member adds its weapon's), a critical when it
/// is STRICTLY greater than the roll.
fn ref_critical_chance(actor: &Combatant, target: &Combatant) -> i64 {
    let base = (i64::from(actor.luck) + i64::from(actor.level) - i64::from(target.level)) >> 2;
    let bonus = if actor.side == Side::Party { i64::from(actor.crit_bonus) } else { 0 };
    (base + bonus).clamp(0, 255)
}

/// Every md1 enemy once (by name), from the formations.
fn distinct_enemies(bin: &SceneBin) -> Vec<Combatant> {
    let mut seen: Vec<Combatant> = Vec::new();
    for &(formation, _) in MD1 {
        for e in enemies_of(bin, formation) {
            if !seen.iter().any(|s| s.name == e.name) {
                seen.push(e);
            }
        }
    }
    seen
}

#[test]
#[ignore = "needs FMV_FF7_DATA pointing at the owner's FF7 data directory"]
fn every_blow_between_the_party_and_the_first_reactors_enemies_follows_the_documented_formulas() {
    let Some((bin, kernel)) = load() else { return };
    let enemies = distinct_enemies(&bin);
    assert_eq!(enemies.len(), RECORDS.len());
    let (mut physical, mut magical, mut cures, mut hits, mut crits) = (0usize, 0usize, 0usize, 0usize, 0usize);
    for level in 5..=8u8 {
        for who in [cloud(&kernel, level), barret(&kernel, level)] {
            for foe in &enemies {
                // ---- the party's Attack command on the enemy ----
                let spec = who.attack_spec();
                assert_eq!((spec.power, spec.accuracy), (16, who.accuracy));
                for (row, defending) in [(Row::Front, false), (Row::Back, false), (Row::Front, true)] {
                    let (mut a, mut t) = (who.clone(), foe.clone());
                    a.row = row;
                    t.defending = defending;
                    for critical in [false, true] {
                        for variance in 0..=255u32 {
                            let mut base = ref_x1(u64::from(a.attack), u64::from(a.level), u64::from(t.defense), 16);
                            if critical {
                                base *= 2;
                            }
                            // "Row: /2" for a short-range attack across the back row (the enemy's row never matters here).
                            if a.short_range && a.row == Row::Back {
                                base /= 2;
                            }
                            if defending {
                                base /= 2;
                            }
                            let want = ref_variance(base, u64::from(variance)).min(9999) as u32;
                            let got = action_damage(&spec, &a, &t, Elemental::Normal, Rolls { critical, variance });
                            assert_eq!(got, want, "{} L{level} on {}: critical {critical}, {row:?}, defending {defending}, variance {variance}", who.name, foe.name);
                            physical += 1;
                        }
                    }
                }
                // ---- the hit and critical rates, roll by roll ----
                let chance = ref_hit_chance(&who, foe, i64::from(who.accuracy));
                assert_eq!(i64::from(hit_chance(&who, foe)), chance, "{} L{level} on {}", who.name, foe.name);
                for luck_roll in 0..=99u32 {
                    for miss_roll in 1..=100u32 {
                        let want = ref_hits(&who, foe, chance, luck_roll, miss_roll);
                        assert_eq!(resolve_hit(&who, foe, chance as i32, luck_roll, miss_roll), want, "{} on {}: luck {luck_roll}, miss {miss_roll}", who.name, foe.name);
                        hits += 1;
                    }
                }
                let crit = ref_critical_chance(&who, foe);
                assert_eq!(i64::from(critical_chance(&who, foe)), crit, "{} L{level} on {}", who.name, foe.name);
                for roll in 1..=100u32 {
                    assert_eq!(is_critical(crit as u32, roll), crit > i64::from(roll));
                    crits += 1;
                }
                // ---- the three bolts: ((Level + Magic) * 6) * (512 - Magic defense) * Power >> 13 ----
                for spell in who.spells.iter().filter(|s| s.index != 0) {
                    for variance in 0..=255u32 {
                        let want = ref_variance(ref_x2(u64::from(who.magic), u64::from(who.level), u64::from(foe.magic_defense), u64::from(spell.spec.power)), u64::from(variance)) as u32;
                        let got = action_damage(&spell.spec, &who, foe, Elemental::Normal, Rolls { critical: false, variance });
                        assert_eq!(got, want, "{} L{level} casts {} on {}: variance {variance}", who.name, spell.index, foe.name);
                        magical += 1;
                    }
                }
            }
            // ---- Cure: 22 * Power + 6 * (Level + Magic), with the variance (X5) ----
            let cure = who.spells.iter().find(|s| s.index == 0).expect("Cure");
            let mut wounded = who.clone();
            wounded.hp = 1;
            for variance in 0..=255u32 {
                let want = ref_variance(22 * u64::from(cure.spec.power) + 6 * (u64::from(who.level) + u64::from(who.magic)), u64::from(variance)) as u32;
                assert_eq!(restore_amount(&cure.spec, &who, &wounded, variance), want, "{} L{level}: variance {variance}", who.name);
                cures += 1;
            }
        }
    }
    // ---- the enemies' own attacks on the canon pair (front row) ----
    let mut enemy_blows = 0usize;
    for foe in &enemies {
        for victim in [cloud(&kernel, 6), barret(&kernel, 6)] {
            assert!(!foe.actions.is_empty(), "{} attacks", foe.name);
            for action in &foe.actions {
                let physical = action.spec.kind == fmv_ff7_battle::rules::ActionKind::Physical;
                let chance = ref_hit_chance(foe, &victim, i64::from(action.spec.accuracy));
                assert_eq!(i64::from(hit_chance_with(foe, &victim, action.spec.accuracy)), chance, "{} #{} on {}", foe.name, action.id, victim.name);
                for luck_roll in 0..=99u32 {
                    for miss_roll in 1..=100u32 {
                        assert_eq!(resolve_hit(foe, &victim, chance as i32, luck_roll, miss_roll), ref_hits(foe, &victim, chance, luck_roll, miss_roll));
                    }
                }
                for critical in [false, true] {
                    for variance in 0..=255u32 {
                        let got = action_damage(&action.spec, foe, &victim, Elemental::Normal, Rolls { critical, variance });
                        let want = if physical {
                            let mut base = ref_x1(u64::from(foe.attack), u64::from(foe.level), u64::from(victim.defense), u64::from(action.spec.power));
                            if critical && action.spec.can_crit {
                                base *= 2;
                            }
                            ref_variance(base, u64::from(variance)) as u32
                        } else {
                            ref_variance(ref_x2(u64::from(foe.magic), u64::from(foe.level), u64::from(victim.magic_defense), u64::from(action.spec.power)), u64::from(variance)) as u32
                        };
                        assert_eq!(got, want, "{} #{} on {}: critical {critical}, variance {variance}", foe.name, action.id, victim.name);
                        enemy_blows += 1;
                    }
                }
            }
        }
    }
    eprintln!("checked {physical} physical blows, {magical} spells, {cures} cures, {hits} hit rolls, {crits} critical rolls and {enemy_blows} enemy blows against the reference");
}

/// The magic hit rule of the pseudocode (two rolls, `acc + (actor's level - level / 2) < rand_1` misses, `M_Evade > rand_0` misses) never
/// misses in the first reactor: every spell either way has a hit rate of 100 and a caster at least as high as the half of its target's
/// level, and nobody there has a magic evade. The engine's simpler rule (the hit rate less the target's magic evade against one roll of
/// 1 to 100) agrees: nothing here ever misses.
#[test]
#[ignore = "needs FMV_FF7_DATA pointing at the owner's FF7 data directory"]
fn spells_never_miss_in_the_first_reactor_by_either_reading_of_the_rule() {
    let Some((bin, kernel)) = load() else { return };
    for level in 5..=8u8 {
        for who in [cloud(&kernel, level), barret(&kernel, level)] {
            for foe in distinct_enemies(&bin) {
                for spell in who.spells.iter().filter(|s| s.index != 0) {
                    // The pseudocode's reading (the half-level being the TARGET's, the only reading that is a rule about a target).
                    let bonus = i64::from(who.level) - i64::from(foe.level) / 2;
                    // `rand_1` is `0..=99`: a spell misses only when `acc + bonus` is under it, so 99 is already a sure hit.
                    assert!(i64::from(spell.spec.accuracy) + bonus >= 99, "{} L{level} {} on {}", who.name, spell.index, foe.name);
                    assert_eq!(foe.magic_evade, 0);
                    assert!(fmv_ff7_battle::rules::magic_hit_chance(spell.spec.accuracy, &foe) >= 100);
                }
            }
        }
    }
    // The enemy side: the one magical attack there is (the Mono Drive's Fire) against the canon pair's armor.
    for foe in distinct_enemies(&bin) {
        for action in foe.actions.iter().filter(|a| a.spec.kind == fmv_ff7_battle::rules::ActionKind::Magical) {
            for victim in [cloud(&kernel, 6), barret(&kernel, 6)] {
                let bonus = i64::from(foe.level) - i64::from(victim.level) / 2;
                assert!(i64::from(action.spec.accuracy) + bonus >= 99 && victim.magic_evade == 0, "{} #{} on {}", foe.name, action.id, victim.name);
                assert!(fmv_ff7_battle::rules::magic_hit_chance(action.spec.accuracy, &victim) >= 100);
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// 4. Rates through the engine's own draws, over thousands of seeded blows
// ---------------------------------------------------------------------------------------------------------------

/// The exact odds of the draws of one `Attack`: `(P(hit), P(critical | hit))` of `who` on `foe`, by counting every value the draws can
/// take (the luck roll `0..=99` uniform, the miss and critical rolls the 16-bit draw mapped by [`roll_of`]).
fn exact_odds(who: &Combatant, foe: &Combatant) -> (f64, f64) {
    let mut counts = [0u32; 101];
    for raw in 0..=65_535u32 {
        counts[roll_of(raw) as usize] += 1;
    }
    let chance = ref_hit_chance(who, foe, i64::from(who.accuracy));
    let mut hit = 0.0;
    for luck_roll in 0..=99u32 {
        for miss_roll in 1..=100u32 {
            if ref_hits(who, foe, chance, luck_roll, miss_roll) {
                hit += f64::from(counts[miss_roll as usize]) / 65_536.0 / 100.0;
            }
        }
    }
    let crit = ref_critical_chance(who, foe);
    let crit_p: f64 = (1..=100u32).filter(|&roll| crit > i64::from(roll)).map(|roll| f64::from(counts[roll as usize]) / 65_536.0).sum();
    (hit, crit_p)
}

#[test]
#[ignore = "needs FMV_FF7_DATA pointing at the owner's FF7 data directory"]
fn the_engine_hits_and_crits_at_the_documented_rates_and_deals_the_documented_damage() {
    let Some((bin, kernel)) = load() else { return };
    const SEEDS: u64 = 12_000;
    // (formation, the enemy attacked first by the one hero, who)
    let matchups: [(u16, &str); 6] = [(301, "MP"), (302, "Guard Hound"), (305, "Mono Drive"), (309, "1st Ray"), (310, "Grunt"), (320, "Sweeper")];
    for (formation, enemy) in matchups {
        for who in [cloud(&kernel, 6), barret(&kernel, 6)] {
            let foe = enemies_of(&bin, formation).into_iter().find(|e| e.name == enemy).expect("the enemy");
            let target = ActorId(1);
            let (p_hit, p_crit) = exact_odds(&who, &foe);
            let base = ref_x1(u64::from(who.attack), u64::from(who.level), u64::from(foe.defense), 16);
            // Every amount a blow can do: the variance over the base, doubled by a critical hit; halved nowhere (front row).
            let normal: Vec<u32> = (0..=255u64).map(|v| ref_variance(base, v) as u32).collect();
            let critical: Vec<u32> = (0..=255u64).map(|v| ref_variance(base * 2, v) as u32).collect();
            let (mut hits, mut crits, mut seen_min, mut seen_max) = (0u64, 0u64, u32::MAX, 0u32);
            for seed in 0..SEEDS {
                let mut state = BattleState::with_config(seed, vec![who.clone()], enemies_of(&bin, formation), BattleConfig { start: StartMode::Preemptive, ..BattleConfig::default() });
                let events = state.submit_command(ActorId(0), Command::Attack { target }).expect("the attack");
                let blow = events.iter().find(|e| matches!(e, BattleEvent::Damage { .. } | BattleEvent::Miss { .. })).expect("a blow");
                if let BattleEvent::Damage { amount, critical: was_crit, .. } = *blow {
                    hits += 1;
                    crits += u64::from(was_crit);
                    let allowed = if was_crit { &critical } else { &normal };
                    // A blow cannot take more than the target had, but the event names what was rolled.
                    assert!(allowed.contains(&amount), "{} on {}: {amount} is not a damage the formula gives (critical {was_crit})", who.name, foe.name);
                    seen_min = seen_min.min(amount);
                    seen_max = seen_max.max(amount);
                }
            }
            let n = SEEDS as f64;
            let (rate_hit, rate_crit) = (hits as f64 / n, crits as f64 / hits.max(1) as f64);
            let tol_hit = 5.0 * (p_hit * (1.0 - p_hit)).max(0.0).sqrt() / n.sqrt() + 1e-9;
            let tol_crit = 5.0 * (p_crit * (1.0 - p_crit)).max(0.0).sqrt() / (hits.max(1) as f64).sqrt() + 1e-9;
            assert!((rate_hit - p_hit).abs() <= tol_hit, "{} on {}: hit rate {rate_hit:.4}, documented {p_hit:.4} (+-{tol_hit:.4})", who.name, foe.name);
            assert!((rate_crit - p_crit).abs() <= tol_crit, "{} on {}: critical rate {rate_crit:.4}, documented {p_crit:.4} (+-{tol_crit:.4})", who.name, foe.name);
            eprintln!(
                "{} L6 on {enemy}: hits {rate_hit:.3} (documented {p_hit:.3}), crits {rate_crit:.3} of hits (documented {p_crit:.3}), damage {seen_min}-{seen_max} (base {base})",
                who.name
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// 5. A battle is won when every enemy is dead
// ---------------------------------------------------------------------------------------------------------------

#[test]
#[ignore = "needs FMV_FF7_DATA pointing at the owner's FF7 data directory"]
fn victory_comes_only_with_the_last_enemy_and_defeat_only_with_the_last_party_member() {
    let Some((bin, kernel)) = load() else { return };
    for &(formation, names) in MD1.iter().filter(|(f, _)| *f != 324) {
        let enemies = enemies_of(&bin, formation);
        let count = enemies.len();
        assert_eq!(count, names.len());
        // A party whose every blow kills (Cloud at 99 with the full-gauge start) takes the enemies one at a time, in every order.
        for order in 0..count {
            let mut hero = cloud(&kernel, 99);
            hero.accuracy = 255;
            let mut state = BattleState::with_config(3, vec![hero], enemies_of(&bin, formation), BattleConfig { start: StartMode::Preemptive, ..BattleConfig::default() });
            let mut killed = 0usize;
            let mut log = Vec::new();
            // Kill them starting from the `order`-th (wrapping), whatever the enemies do in between.
            for step in 0..count {
                let id = ActorId((1 + (order + step) % count) as u8);
                for _ in 0..5_000 {
                    if !state.ready_actors().is_empty() || state.outcome().is_some() {
                        break;
                    }
                    log.extend(state.step_ticks(4));
                }
                if state.outcome().is_some() {
                    break;
                }
                let events = state.submit_command(ActorId(0), Command::Attack { target: id }).expect("a living enemy");
                killed += 1;
                let dead = state.enemy_ids().iter().filter(|e| !state.actor(**e).is_some_and(Combatant::is_alive)).count();
                assert_eq!(dead, killed, "formation {formation}: every blow of this hero kills");
                let victory = events.iter().filter(|e| matches!(e, BattleEvent::Victory)).count();
                if killed < count {
                    assert_eq!((victory, state.outcome()), (0, None), "formation {formation}: {killed} of {count} enemies are dead, the battle is not won");
                } else {
                    assert_eq!((victory, state.outcome()), (1, Some(Outcome::Victory)), "formation {formation}: the last enemy ends it, once");
                    assert_eq!(events.last(), Some(&BattleEvent::Victory));
                }
                log.extend(events);
            }
            assert!(!log.iter().any(|e| matches!(e, BattleEvent::Defeat)), "formation {formation}");
            assert_eq!(state.outcome(), Some(Outcome::Victory), "formation {formation}, order {order}");
        }
    }
}
