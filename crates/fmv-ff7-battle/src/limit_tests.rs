//! Tests of the limit gauge, the Limit command and Escape in the battle engine (the rules are in
//! [`crate::limit`] and [`crate::rules`]; the hand-computed numbers are in their docs).

use crate::action::{ActionSpec, Aim, Formula};
use crate::limit::{LimitBreak, LimitInfo, LIMIT_MAX};
use crate::rules::*;

fn config(start: StartMode) -> BattleConfig {
    BattleConfig { start, ..BattleConfig::default() }
}

/// A hero with 400 max HP and a limit (id 128, `Power` 48) whose level-1 divisor is 140: a hit of 100
/// damage adds `100 * 76800 / (400 * 140)` = 137 to its gauge.
fn hero(limit: u16) -> Combatant {
    let mut c = Combatant::new("Hero", 10, 400);
    c.attack = 60;
    c.defense = 20;
    c.magic = 12;
    c.dex = 40;
    c.limit_break = Some(LimitBreak::new(128, ActionSpec::limit_strike(48), 140));
    c.limit = limit;
    c
}

/// Like [`hero`], but its limit heals the whole party, each ally for half of its max HP.
fn healer(limit: u16) -> Combatant {
    let mut c = hero(limit);
    c.name = "Healer".into();
    c.limit_break = Some(LimitBreak::new(142, ActionSpec::limit_heal(16), 140));
    c
}

/// An enemy whose only attack deals exactly `20 * power` damage and never misses (a flat formula with
/// no variance, no defense and no roll to hit), so the numbers of a test are exact.
fn foe(hp: u32, power: u32) -> Combatant {
    let mut c = Combatant::new("Foe", 8, hp);
    c.dex = 40;
    c.defense = 10;
    c.actions = vec![EnemyAction { id: 1, spec: ActionSpec { formula: Formula::Flat, ..ActionSpec::damage_item(power, 0) } }];
    c
}

const A: ActorId = ActorId(0);

/// The battle with the events of its construction delivered (a preemptive start announces the full
/// gauges with the first step or command), so that what a command returns is exactly its own events.
fn settled(mut battle: BattleState) -> BattleState {
    let _ = battle.step_ticks(0);
    battle
}

/// Step until `n` blows of the enemy have landed and return everything that happened.
fn until_hits(battle: &mut BattleState, n: usize) -> Vec<BattleEvent> {
    let mut log = Vec::new();
    let mut hits = 0;
    for _ in 0..10_000 {
        let events = battle.step_ticks(50);
        hits += events.iter().filter(|e| matches!(e, BattleEvent::Damage { .. })).count();
        log.extend(events);
        if hits >= n || battle.outcome().is_some() {
            break;
        }
    }
    log
}

fn gauges(log: &[BattleEvent]) -> Vec<(u8, u16)> {
    log.iter().filter_map(|e| if let BattleEvent::LimitGauge { actor, value } = e { Some((actor.0, *value)) } else { None }).collect()
}

#[test]
fn a_blow_of_an_enemy_fills_the_gauge_by_the_documented_rule_and_announces_it() {
    let mut battle = BattleState::with_config(1, vec![hero(0)], vec![foe(900, 5)], config(StartMode::BackAttack));
    let log = until_hits(&mut battle, 1);
    // The enemy acts at once: its action, its blow of 100 and the gauge that blow raised, in that order.
    let at = log.iter().position(|e| matches!(e, BattleEvent::Action { .. })).expect("the enemy acted");
    assert_eq!(log[at], BattleEvent::Action { actor: ActorId(1), action: ActionRef::EnemyAttack(1) });
    assert_eq!(log[at + 1], BattleEvent::Damage { actor: ActorId(1), target: A, amount: 100, critical: false });
    assert_eq!(log[at + 2], BattleEvent::LimitGauge { actor: A, value: 137 }, "100 * 76800 / (400 * 140)");
    assert_eq!(battle.limit_gauge(A), 137);
    assert!(!battle.limit_ready(A));
    // The second blow fills it (274 is capped at a full gauge); the third changes nothing, so says nothing.
    let log = until_hits(&mut battle, 1);
    assert_eq!(gauges(&log), [(0, LIMIT_MAX)]);
    assert!(battle.limit_ready(A));
    let log = until_hits(&mut battle, 1);
    assert!(gauges(&log).is_empty(), "a full gauge cannot rise: {log:?}");
    assert_eq!(battle.limit_gauge(A), LIMIT_MAX);
}

#[test]
fn defending_halves_what_the_blow_adds() {
    // A quick enemy: it strikes (after about 220 ticks) while the guard still holds (the hero needs about
    // 360 to be ready again, and the guard ends with its next turn).
    let mut quick = foe(900, 5);
    quick.dex = 150;
    let mut battle = BattleState::with_config(1, vec![hero(0)], vec![quick], config(StartMode::Preemptive));
    battle.submit_command(A, Command::Defend).expect("defend");
    let log = until_hits(&mut battle, 1);
    assert!(log.contains(&BattleEvent::Damage { actor: ActorId(1), target: A, amount: 50, critical: false }), "{log:?}");
    assert_eq!(gauges(&log), [(0, 68)], "50 * 76800 / 56000 = 68");
}

#[test]
fn only_an_enemys_blow_on_a_party_member_with_a_limit_fills_a_gauge() {
    // No limit break, no gauge, whatever the starting value says; and an enemy never has one.
    let mut plain = hero(200);
    plain.limit_break = None;
    let mut proud = foe(900, 5);
    proud.limit = 200;
    proud.limit_break = Some(LimitBreak::new(128, ActionSpec::limit_strike(48), 100));
    let mut battle = BattleState::with_config(1, vec![plain, hero(0)], vec![proud], config(StartMode::BackAttack));
    assert_eq!([A, ActorId(2)].map(|id| battle.limit_gauge(id)), [0, 0]);
    let log = until_hits(&mut battle, 4);
    assert!(gauges(&log).iter().all(|(actor, _)| *actor == 1), "only the hero with a limit: {log:?}");
    assert_eq!(battle.limit_gauge(A), 0);
    assert_eq!(battle.limit_for(ActorId(2)), None, "an enemy has no limit to offer");
    // The party's own healing or the hero's own damage is not a blow of an enemy.
    let mut cure = hero(0);
    cure.spells = vec![Spell::new(0, ActionSpec::cure_spell(5, 0))];
    let mut ally = hero(0);
    ally.hp = 100;
    let mut battle = BattleState::with_config(1, vec![cure, ally], vec![foe(900, 5)], config(StartMode::Preemptive));
    let events = battle.submit_command(A, Command::Magic { spell: 0, target: Target::One(ActorId(1)) }).expect("cure");
    assert!(gauges(&events).is_empty() && battle.limit_gauge(ActorId(1)) == 0);
}

#[test]
fn a_starting_gauge_is_kept_but_never_above_full_and_only_for_the_living_with_a_limit() {
    let mut dead = hero(255);
    dead.hp = 0;
    let battle = BattleState::with_config(1, vec![hero(100), hero(9999), dead], vec![foe(900, 5)], config(StartMode::Zero));
    assert_eq!([0, 1, 2].map(|i| battle.limit_gauge(ActorId(i))), [100, LIMIT_MAX, 0]);
    assert!(!battle.limit_ready(ActorId(0)) && battle.limit_ready(ActorId(1)) && !battle.limit_ready(ActorId(2)));
    assert_eq!(battle.limit_gauge(ActorId(77)), 0, "an unknown actor has no gauge");
    assert_eq!(battle.limit_for(ActorId(1)), Some(LimitInfo { attack: 128, targets_allies: false, aim: Aim::One }));
}

#[test]
fn the_limit_command_needs_a_limit_a_full_gauge_an_open_turn_and_a_target_on_the_right_side() {
    // A hero that has none, one whose gauge is short, one that is ready, with every ATB gauge full.
    let mut plain = hero(0);
    plain.limit_break = None;
    let mut battle = settled(BattleState::with_config(1, vec![plain, hero(254), hero(255)], vec![foe(900, 5)], config(StartMode::Preemptive)));
    let enemy = ActorId(3);
    let limit = |target| Command::Limit { target: Target::One(target) };
    assert_eq!(battle.check_command(A, &limit(enemy)), Err(CommandError::NoLimit(A)));
    assert_eq!(battle.check_command(ActorId(1), &limit(enemy)), Err(CommandError::LimitNotReady(ActorId(1))));
    assert_eq!(battle.check_command(ActorId(2), &limit(ActorId(0))), Err(CommandError::InvalidTarget(ActorId(0))), "a damaging limit aims at an enemy");
    assert_eq!(battle.check_command(ActorId(2), &limit(ActorId(9))), Err(CommandError::UnknownTarget(ActorId(9))));
    assert_eq!(battle.check_command(ActorId(2), &limit(enemy)), Ok(()));
    // A refusal changes nothing.
    let before = battle.clone();
    assert!(battle.submit_command(ActorId(1), limit(enemy)).is_err());
    assert_eq!(battle, before);

    // Used: the action, the empty gauge, then the blow (never a miss, standard formula with variance).
    let events = battle.submit_command(ActorId(2), limit(enemy)).expect("the limit");
    assert_eq!(events[0], BattleEvent::Action { actor: ActorId(2), action: ActionRef::Limit(128) });
    assert_eq!(events[1], BattleEvent::LimitGauge { actor: ActorId(2), value: 0 });
    let BattleEvent::Damage { actor, target, amount, critical } = events[2] else { panic!("a blow: {events:?}") };
    assert_eq!((actor, target, critical, events.len()), (ActorId(2), enemy, false, 3));
    let blow = |variance_roll| {
        physical_damage(&DamageInput {
            attack: 60,
            level: 10,
            power: 48,
            defense: 10,
            critical: false,
            halve_for_row: false,
            defending: false,
            elemental: Elemental::Normal,
            variance_roll,
        })
    };
    assert!((blow(0)..=blow(255)).contains(&amount), "{amount} outside {}..={}", blow(0), blow(255));
    assert_eq!(battle.actor(enemy).unwrap().hp, 900 - amount);
    assert_eq!(battle.limit_gauge(ActorId(2)), 0);
    // The turn is over like any command's, and the gauge has to be filled again.
    assert_eq!(battle.check_command(ActorId(2), &limit(enemy)), Err(CommandError::GaugeNotFull(ActorId(2))));
}

#[test]
fn a_healing_limit_restores_half_of_every_living_allys_max_hp_and_only_aims_at_the_whole_party() {
    let mut hurt = hero(0);
    hurt.hp = 100;
    let mut dead = hero(0);
    dead.hp = 0;
    let mut almost = hero(0);
    almost.hp = 350;
    let mut battle = settled(BattleState::with_config(1, vec![healer(255), hurt, dead, almost], vec![foe(900, 5)], config(StartMode::Preemptive)));
    assert_eq!(battle.limit_for(A), Some(LimitInfo { attack: 142, targets_allies: true, aim: Aim::Side }));
    // It is the whole party or nothing: one ally, one enemy or the enemies are no way to aim it.
    for target in [Target::One(ActorId(1)), Target::One(A), Target::One(ActorId(4)), Target::AllEnemies] {
        assert_eq!(battle.check_command(A, &Command::Limit { target }), Err(CommandError::InvalidReach(target)), "{target:?}");
    }
    let wind = Command::Limit { target: Target::AllAllies };
    assert_eq!(battle.check_command(A, &wind), Ok(()));
    let events = battle.submit_command(A, wind).expect("heal everyone");
    // Everyone who is alive, in actor order, each for half of its own max HP (400 / 2) and never more than
    // it was missing; the fallen are not touched. Not split: a fraction of max HP never is.
    assert_eq!(
        events,
        [
            BattleEvent::Action { actor: A, action: ActionRef::Limit(142) },
            BattleEvent::LimitGauge { actor: A, value: 0 },
            BattleEvent::Heal { actor: A, target: A, amount: 0 },
            BattleEvent::Heal { actor: A, target: ActorId(1), amount: 200 },
            BattleEvent::Heal { actor: A, target: ActorId(3), amount: 50 },
        ]
    );
    assert_eq!([A, ActorId(1), ActorId(2), ActorId(3)].map(|id| battle.actor(id).unwrap().hp), [400, 300, 0, 400]);
    // A party with every ally fallen but the healer still has someone to heal: the healer.
    let mut alone = settled(BattleState::with_config(1, vec![healer(255)], vec![foe(900, 5)], config(StartMode::Preemptive)));
    let events = alone.submit_command(A, wind).expect("the healer heals itself");
    assert_eq!(events.last(), Some(&BattleEvent::Heal { actor: A, target: A, amount: 0 }));
}

#[test]
fn a_knockout_empties_the_gauge_and_says_so_before_the_battle_ends() {
    // 600 damage kills the lone hero: the blow, the death, the gauge that went with it, then the defeat.
    let mut battle = BattleState::with_config(1, vec![hero(200)], vec![foe(900, 30)], config(StartMode::BackAttack));
    let log = until_hits(&mut battle, 1);
    let tail = &log[log.len() - 4..];
    assert_eq!(
        tail,
        [
            BattleEvent::Damage { actor: ActorId(1), target: A, amount: 600, critical: false },
            BattleEvent::Death { actor: A },
            BattleEvent::LimitGauge { actor: A, value: 0 },
            BattleEvent::Defeat,
        ]
    );
    assert_eq!((battle.limit_gauge(A), battle.outcome()), (0, Some(Outcome::Defeat)));
    // A character with nothing in its gauge has nothing to announce when it dies.
    let mut battle = BattleState::with_config(1, vec![hero(0)], vec![foe(900, 30)], config(StartMode::BackAttack));
    assert!(gauges(&until_hits(&mut battle, 1)).is_empty());
}

#[test]
fn a_limit_that_ends_the_battle_still_ends_it_on_the_victory() {
    let mut battle = settled(BattleState::with_config(1, vec![hero(255)], vec![foe(10, 5)], config(StartMode::Preemptive)));
    let events = battle.submit_command(A, Command::Limit { target: Target::One(ActorId(1)) }).expect("the limit");
    assert_eq!(events[1], BattleEvent::LimitGauge { actor: A, value: 0 }, "the gauge is spent before the effects");
    assert_eq!(events.last(), Some(&BattleEvent::Victory));
    assert_eq!(battle.outcome(), Some(Outcome::Victory));
}

#[test]
fn a_sure_escape_gets_away_at_once_pays_nothing_and_ends_the_battle() {
    let mut battle = BattleState::with_config(1, vec![hero(0), hero(0)], vec![foe(900, 5)], config(StartMode::Zero))
        .with_escape(Some(EscapeRule { difficulty: 3, sure: true }));
    assert!(battle.can_escape());
    assert_eq!(battle.escape_left(), Some(3));
    // Nobody can act yet: the actor's turn comes first.
    assert_eq!(battle.check_command(A, &Command::Escape), Err(CommandError::GaugeNotFull(A)));
    let mut ready = None;
    for _ in 0..10_000 {
        battle.step_ticks(20);
        ready = battle.ready_actors().first().copied();
        if ready.is_some() {
            break;
        }
    }
    let actor = ready.expect("somebody's gauge fills");
    let events = battle.submit_command(actor, Command::Escape).expect("sure to get away: four checks bring 3 down to 0");
    assert_eq!(events.last(), Some(&BattleEvent::Action { actor, action: ActionRef::Escape }));
    assert!(!events.iter().any(|e| matches!(e, BattleEvent::EscapeFailed { .. } | BattleEvent::Victory | BattleEvent::Defeat)));
    assert_eq!((battle.outcome(), battle.escape_left()), (Some(Outcome::Escaped), Some(0)));
    // Over: nothing more happens and nothing more is accepted.
    assert!(battle.step(10_000).is_empty());
    assert_eq!(battle.check_command(actor, &Command::Escape), Err(CommandError::BattleOver));
    // Nobody was paid: the enemy is alive, so the engine's total is empty.
    assert_eq!(battle.rewards(), Reward::default());
}

#[test]
fn a_battle_with_no_escape_rule_cannot_be_run_from_and_refusing_changes_nothing() {
    let mut battle = BattleState::with_config(1, vec![hero(0)], vec![foe(900, 5)], config(StartMode::Preemptive)).with_escape(None);
    assert!(!battle.can_escape());
    assert_eq!(battle.escape_left(), None);
    let before = battle.clone();
    assert_eq!(battle.check_command(A, &Command::Escape), Err(CommandError::CannotEscape));
    assert_eq!(battle.submit_command(A, Command::Escape), Err(CommandError::CannotEscape));
    assert_eq!(battle, before, "no draw, no turn spent");
    // A rule of difficulty 0 means 1, never a battle that is already over.
    let zero = BattleState::with_config(1, vec![hero(0)], vec![foe(900, 5)], config(StartMode::Zero)).with_escape(Some(EscapeRule { difficulty: 0, sure: false }));
    assert_eq!(zero.escape_left(), Some(1));
    // A battle built without a rule can be run from at difficulty 1, surely when the party struck first.
    let default = BattleState::new(1, vec![hero(0)], vec![foe(900, 5)]);
    assert_eq!(default.escape_left(), Some(1));
    let mut pre = BattleState::with_config(1, vec![hero(0)], vec![foe(900, 5)], config(StartMode::Preemptive));
    pre.submit_command(A, Command::Escape).expect("a preemptive start is a sure escape");
    assert_eq!(pre.outcome(), Some(Outcome::Escaped));
}

/// One escape attempt of a fresh battle with `rule`, with seed `seed`; the engine's state afterwards.
fn attempt(seed: u64, rule: EscapeRule) -> (Vec<BattleEvent>, BattleState) {
    let mut battle = settled(BattleState::with_config(seed, vec![hero(0)], vec![foe(900, 5)], config(StartMode::Preemptive)).with_escape(Some(rule)));
    let events = battle.submit_command(A, Command::Escape).expect("an attempt");
    (events, battle)
}

#[test]
fn run_checks_succeed_one_in_four_four_to_a_turn_and_what_is_left_is_kept() {
    let rate = |difficulty: u8| {
        (0..4000u64).filter(|seed| attempt(*seed, EscapeRule { difficulty, sure: false }).1.outcome() == Some(Outcome::Escaped)).count()
    };
    // Difficulty 1: at least one of four checks at 1 in 4 (68.4%); difficulty 2: at least two (26.2%).
    let (one, two) = (rate(1), rate(2));
    assert!((2580..2890).contains(&one), "difficulty 1: {one} of 4000 (expected about 2734)");
    assert!((900..1200).contains(&two), "difficulty 2: {two} of 4000 (expected about 1047)");
    assert!(rate(4) < 60, "four successes in four checks: 0.4%");

    // A failure is announced, spends the turn, and keeps the progress the checks made.
    let mut halfway = None;
    for seed in 0..400u64 {
        let (events, battle) = attempt(seed, EscapeRule { difficulty: 2, sure: false });
        if battle.outcome().is_some() {
            assert!(!events.iter().any(|e| matches!(e, BattleEvent::EscapeFailed { .. })));
            continue;
        }
        assert_eq!(
            events,
            [BattleEvent::Action { actor: A, action: ActionRef::Escape }, BattleEvent::EscapeFailed { actor: A }]
        );
        assert!(battle.ready_actors().is_empty(), "a failed attempt ends the turn");
        if battle.escape_left() == Some(1) && halfway.is_none() {
            halfway = Some(battle);
        }
    }
    let mut battle = halfway.expect("some seed gets one of the two checks it needs and then fails");
    // The rest of the difficulty is what the next attempt has to bring down: at 1 it is the 68% case.
    let mut left = battle.escape_left();
    for _ in 0..200 {
        for _ in 0..10_000 {
            if !battle.ready_actors().is_empty() || battle.outcome().is_some() {
                break;
            }
            battle.step_ticks(20);
        }
        if battle.outcome().is_some() {
            break;
        }
        let events = battle.submit_command(A, Command::Escape).expect("another attempt");
        assert!(battle.escape_left() <= left, "the difficulty never rises again: {events:?}");
        left = battle.escape_left();
    }
    assert_eq!(battle.outcome(), Some(Outcome::Escaped), "a party that keeps trying gets away");
}

#[test]
fn escape_attempts_draw_in_the_documented_order() {
    // Not sure: up to four draws of `below(4)` from the battle's own stream, stopping once away. Compare with
    // a generator of the same seed run by hand.
    for seed in 0..50u64 {
        let (_, battle) = attempt(seed, EscapeRule { difficulty: 2, sure: false });
        let mut rng = crate::rng::Rng::new(seed);
        // The battle's start (a pre-emptive one here) dealt each of its two combatants a starting timer first.
        for _ in 0..2 {
            rng.range_inclusive(0, INITIAL_GAUGE_MAX);
        }
        let mut left = 2u8;
        for _ in 0..RUN_CHECKS_PER_TURN {
            if left == 0 {
                break;
            }
            if rng.below(RUN_CHECK_ODDS) == 0 {
                left -= 1;
            }
        }
        assert_eq!(battle.escape_left(), Some(left), "seed {seed}");
        assert_eq!(battle.outcome() == Some(Outcome::Escaped), left == 0, "seed {seed}");
    }
    // Sure: nothing is drawn, so a sure attempt leaves the stream where it was.
    let (_, sure) = attempt(5, EscapeRule { difficulty: 1, sure: true });
    assert_eq!(sure.outcome(), Some(Outcome::Escaped));
}

/// Play a battle in which every ready character picks its command from a seeded generator, the way
/// a hostile or confused client might: attack, limit, defend, escape, with random targets.
fn random_battle(seed: u64, rule: Option<EscapeRule>) -> (BattleState, Vec<BattleEvent>) {
    let mut hurt = hero(0);
    hurt.hp = 150;
    let mut battle = BattleState::with_config(
        seed,
        vec![hero(255), healer(255), hurt],
        vec![foe(2500, 4), foe(2500, 4)],
        config(StartMode::Normal),
    )
    .with_escape(rule);
    let mut pick = crate::rng::Rng::with_stream(seed, 0x4C49_4D49);
    let mut log = Vec::new();
    for _ in 0..4000 {
        if battle.outcome().is_some() {
            break;
        }
        log.extend(battle.step_ticks(40));
        for id in battle.ready_actors() {
            let target = ActorId(pick.below(5) as u8);
            // One actor, or either whole side: the healer's limit is a whole side only.
            let aim = match pick.below(4) {
                0 => Target::AllEnemies,
                1 => Target::AllAllies,
                _ => Target::One(target),
            };
            let command = match pick.below(10) {
                0..=3 => Command::Attack { target },
                4..=7 => Command::Limit { target: aim },
                8 => Command::Defend,
                _ => Command::Escape,
            };
            let checked = battle.check_command(id, &command);
            let before = battle.clone();
            match battle.submit_command(id, command) {
                Ok(events) => {
                    assert_eq!(checked, Ok(()), "{command:?}");
                    log.extend(events);
                }
                Err(e) => {
                    assert_eq!(checked, Err(e), "{command:?}");
                    assert_eq!(battle, before, "a refused command changes nothing: {command:?}");
                }
            }
            for (i, c) in battle.actors().iter().enumerate() {
                assert!(c.limit <= LIMIT_MAX, "actor {i}");
                assert!(c.limit == 0 || (c.side == Side::Party && c.is_alive() && c.limit_break.is_some()), "actor {i} has a gauge it cannot have");
            }
            if battle.outcome().is_some() {
                break;
            }
        }
    }
    (battle, log)
}

#[test]
fn random_limit_and_escape_commands_stay_sane_and_replay_exactly() {
    let (mut limits, mut escapes, mut fails, mut gains) = (0, 0, 0, 0);
    for seed in 0..60u64 {
        let rule = (seed % 3 != 0).then_some(EscapeRule { difficulty: 1 + (seed % 3) as u8, sure: false });
        let (a, log_a) = random_battle(seed, rule);
        let (b, log_b) = random_battle(seed, rule);
        assert_eq!((&a, &log_a), (&b, &log_b), "seed {seed}: the same inputs, the same battle");
        assert!(a.outcome().is_some(), "seed {seed} never ended");
        // The terminal event of a victory or a defeat is the last; an escape has none of its own.
        match a.outcome() {
            Some(Outcome::Victory) => assert_eq!(log_a.last(), Some(&BattleEvent::Victory)),
            Some(Outcome::Defeat) => assert_eq!(log_a.last(), Some(&BattleEvent::Defeat)),
            _ => assert!(!log_a.iter().any(|e| matches!(e, BattleEvent::Victory | BattleEvent::Defeat))),
        }
        limits += log_a.iter().filter(|e| matches!(e, BattleEvent::Action { action: ActionRef::Limit(_), .. })).count();
        escapes += log_a.iter().filter(|e| matches!(e, BattleEvent::Action { action: ActionRef::Escape, .. })).count();
        fails += log_a.iter().filter(|e| matches!(e, BattleEvent::EscapeFailed { .. })).count();
        gains += log_a.iter().filter(|e| matches!(e, BattleEvent::LimitGauge { value, .. } if *value > 0)).count();
        if rule.is_none() {
            assert_ne!(a.outcome(), Some(Outcome::Escaped), "seed {seed}: no way out");
        }
    }
    assert!(limits > 20 && escapes > 20 && fails > 5 && gains > 20, "{limits} limits, {escapes} escapes, {fails} failed, {gains} gauge gains");
}

#[test]
fn the_new_events_commands_and_outcome_roundtrip_through_json() {
    let events = [
        BattleEvent::LimitGauge { actor: ActorId(1), value: 137 },
        BattleEvent::EscapeFailed { actor: ActorId(0) },
        BattleEvent::Action { actor: ActorId(0), action: ActionRef::Limit(128) },
        BattleEvent::Action { actor: ActorId(2), action: ActionRef::Escape },
    ];
    for event in events {
        let text = serde_json::to_string(&event).unwrap();
        assert_eq!(serde_json::from_str::<BattleEvent>(&text).unwrap(), event, "{text}");
    }
    for command in [Command::Limit { target: Target::One(ActorId(4)) }, Command::Escape] {
        let text = serde_json::to_string(&command).unwrap();
        assert_eq!(serde_json::from_str::<Command>(&text).unwrap(), command, "{text}");
    }
    assert_eq!(serde_json::to_string(&Outcome::Escaped).unwrap(), r#""Escaped""#);
}
