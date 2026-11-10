//! Tests of the status effects of the battle engine (checkpoint 2 of "deeper battles"): what a combatant starts
//! with, what its statuses do each tick (Poison, Regen, the timers), who may take a turn (Sleep, Stop, Paralysis,
//! Silence), what the statuses change in a blow (Barrier, Sadness, Fury, Darkness, the limit gauge), what an action
//! with a status effect draws, and the items that cure. The model and its sources are in [`crate::status`], the
//! way the rules play it in the module docs of [`crate::rules`] ("Statuses", "RNG draw order"). A child module of
//! `rules`, so that it can replay the draws an action is documented to make and resolve an action directly.
//!
//! The numbers worked by hand, at the default Battle Speed of 128 (V-timer increase 182 a tick, so 91 on the
//! Global Timer): Poison is due every 40960 / 182 = 225.05 ticks, so on the 226th and every 225th or 226th after;
//! Regen every 16384 / 182 = 90.02, so on the 91st; Stop's 122880 Global points last 1350.3 ticks (the 1351st
//! ends it), Paralysis's 65536 last 720.2 (the 721st); Sleep's 491520 V points last 2700.7 ticks (the 2701st) and
//! half of that, or twice, under Haste or Slow. A hero of Dex 40 alone has the normal speed 90, so its gauge
//! gains 182 a tick and fills on the 361st (181st under Haste, 721st under Slow).

use super::*;
use crate::action::{StatusEffect, StatusMode};
use crate::scene::SceneBin;
use crate::status::{BARRIER_DURATION, MODELLED, SLEEP_DURATION, STOP_DURATION};
use crate::testutil::{demo_scene, gzip, pack_scene_bin};
use std::collections::BTreeSet;

const A: ActorId = ActorId(0);
const B: ActorId = ActorId(1);
const C: ActorId = ActorId(2);

fn zero_start() -> BattleConfig {
    BattleConfig { start: StartMode::Zero, ..BattleConfig::default() }
}

fn set(statuses: &[Status]) -> StatusSet {
    statuses.iter().copied().collect()
}

fn fire() -> Spell {
    Spell::new(27, ActionSpec::damage_spell(8, 100, element::FIRE, 4))
}

fn cure() -> Spell {
    Spell::new(0, ActionSpec::cure_spell(5, 5))
}

/// Level 10, 640 HP (a 32nd is 20), Strength 60, Magic 30, Dex 40, Fire and Cure.
fn hero() -> Combatant {
    let mut c = Combatant::new("Hero", 10, 640);
    c.mp = 100;
    c.max_mp = 100;
    c.attack = 60;
    c.magic = 30;
    c.defense = 10;
    c.magic_defense = 10;
    c.dex = 40;
    c.spells = vec![cure(), fire()];
    c
}

/// An enemy that never acts (Dex 0).
fn brute(hp: u32) -> Combatant {
    let mut c = Combatant::new("Brute", 10, hp);
    c.attack = 60;
    c.magic = 30;
    c.defense = 8;
    c.magic_defense = 8;
    c
}

fn with_status(mut c: Combatant, statuses: &[Status]) -> Combatant {
    c.status = StatusState::starting(set(statuses));
    c
}

/// The cures and curses the synthetic kernel does not have (its Antidote has no chance byte, so it stays a record
/// the rules do not play): 0 a Potion, 8 Antidote, 9 Echo Screen, 10 Eye Drop, 11 Remedy (several at once), 12 a
/// whole-party Remedy, 13 a Speed Drink (a buff), 14 a Dazers (a curse).
fn items() -> Arc<ItemTable> {
    let cures = |list: &[Status]| StatusEffect { set: set(list), chance: 63, mode: StatusMode::Cure };
    let remedy = ActionSpec::status_item(cures(&[Status::Sleep, Status::Poison, Status::Silence, Status::Darkness]), true);
    Arc::new(ItemTable::new([
        (0, ItemData::new(ActionSpec::flat_restore_item(5, false))),
        (8, ItemData::new(ActionSpec::status_item(StatusEffect::cure(Status::Poison, 63), true))),
        (9, ItemData::new(ActionSpec::status_item(StatusEffect::cure(Status::Silence, 63), true))),
        (10, ItemData::new(ActionSpec::status_item(StatusEffect::cure(Status::Darkness, 63), true))),
        (11, ItemData::new(remedy)),
        (12, ItemData::new(remedy.with_aim(Aim::Side))),
        (13, ItemData::new(ActionSpec::status_item(StatusEffect::inflict(Status::Haste, 63), true))),
        (14, ItemData::new(ActionSpec::status_item(StatusEffect::inflict(Status::Sleep, 63), false))),
    ]))
}

fn battle(party: Vec<Combatant>, enemies: Vec<Combatant>, seed: u64) -> BattleState {
    BattleState::with_config(seed, party, enemies, zero_start()).with_items(items())
}

fn wait_ready(state: &mut BattleState, who: ActorId) {
    for _ in 0..5000 {
        if state.actor(who).is_some_and(|c| c.ready) {
            return;
        }
        state.step(100);
    }
    panic!("{who:?} never became ready");
}

/// Step one tick at a time until `matches` is among the events, and say on which tick (counted from 1) it was.
fn tick_of(state: &mut BattleState, limit: u64, matches: impl Fn(&BattleEvent) -> bool) -> Option<u64> {
    (1..=limit).find(|_| state.step_ticks(1).iter().any(&matches))
}

fn ended(target: ActorId, status: Status) -> BattleEvent {
    BattleEvent::StatusEnded { target, status }
}

fn inflicted(target: ActorId, status: Status) -> BattleEvent {
    BattleEvent::StatusInflicted { target, status }
}

fn tick(target: ActorId, status: Status, amount: u32) -> BattleEvent {
    BattleEvent::StatusTick { target, status, amount }
}

/// The `StatusTick` events among `events`.
fn ticks_of(events: &[BattleEvent]) -> Vec<BattleEvent> {
    events.iter().filter(|e| matches!(e, BattleEvent::StatusTick { .. })).copied().collect()
}

fn damage_of(events: &[BattleEvent]) -> Option<u32> {
    events.iter().find_map(|e| if let BattleEvent::Damage { amount, .. } = e { Some(*amount) } else { None })
}

/// A blow that always connects: the Attack command's Power 16 at an accuracy of 255.
fn blow() -> ActionSpec {
    ActionSpec::basic_attack(16, 255, false)
}

/// A spell cast by an enemy that always connects.
fn bolt() -> ActionSpec {
    ActionSpec::damage_spell(8, 255, element::FIRE, 0)
}

// --- what a combatant has -----------------------------------------------------------------------------------------

#[test]
fn starting_statuses_are_kept_if_modelled_announced_in_actor_and_bit_order_and_given_their_timers() {
    let hurt = with_status(hero(), &[Status::Haste, Status::Poison, Status::Confusion, Status::NearDeath, Status::Death]);
    let mut fallen = hero();
    fallen.hp = 0;
    fallen.status = StatusState::starting(set(&[Status::Poison]));
    let sleeper = with_status(brute(900), &[Status::Barrier, Status::Sleep, Status::Frog]);
    let mut state = battle(vec![hurt, fallen], vec![sleeper], 4);
    // Only what the rules play stays, and only on the living; the fallen have Death and nothing else.
    assert_eq!(state.statuses(A), set(&[Status::Poison, Status::Haste]));
    assert_eq!(state.statuses(B), set(&[Status::Death]));
    assert!(state.actor(B).unwrap().status.is_empty());
    assert_eq!(state.statuses(C), set(&[Status::Sleep, Status::Barrier]));
    // The timed ones start with their whole duration.
    let enemy = &state.actor(C).unwrap().status;
    assert_eq!((enemy.points_left(Status::Sleep), enemy.points_left(Status::Barrier)), (Some(SLEEP_DURATION), Some(BARRIER_DURATION)));
    // They are announced with the first step, in actor order and then bit order, and once.
    assert_eq!(
        state.step_ticks(0),
        [inflicted(A, Status::Poison), inflicted(A, Status::Haste), inflicted(C, Status::Sleep), inflicted(C, Status::Barrier)]
    );
    assert!(state.step_ticks(0).is_empty());
}

#[test]
fn death_and_near_death_are_read_off_the_hit_points() {
    let mut c = with_status(hero(), &[Status::Poison]);
    c.max_hp = 500;
    c.hp = 500;
    assert_eq!(c.statuses(), set(&[Status::Poison]));
    c.hp = 126;
    assert!(!c.has_status(Status::NearDeath), "126 of 500 is just over a quarter");
    c.hp = 125;
    assert_eq!(c.statuses(), set(&[Status::Poison, Status::NearDeath]));
    c.hp = 1;
    assert!(c.has_status(Status::NearDeath) && c.can_act());
    // A fallen combatant has Death and nothing else, whatever it was carrying.
    c.hp = 0;
    assert_eq!(c.statuses(), set(&[Status::Death]));
    assert!(!c.can_act());
    // Neither is ever stored: they are read off the hit points.
    let state = battle(vec![hero()], vec![brute(10)], 1);
    assert!(state.actor(A).unwrap().status.is_empty());
    let mut near = hero();
    near.hp = 100;
    let state = battle(vec![near], vec![brute(10)], 1);
    assert_eq!(state.statuses(A), set(&[Status::NearDeath]));
    assert!(state.actor(A).unwrap().status.is_empty());
    assert_eq!(state.statuses(ActorId(9)), StatusSet::EMPTY, "nobody there has nothing");
}

// --- Poison and Regen ---------------------------------------------------------------------------------------------

#[test]
fn poison_takes_a_32nd_of_the_maximum_every_two_and_a_half_units_whatever_the_defense() {
    let mut tough = with_status(hero(), &[Status::Poison, Status::Barrier, Status::MBarrier]);
    tough.defense = 255;
    tough.magic_defense = 255;
    let mut state = battle(vec![tough], vec![brute(5000)], 1);
    state.step_ticks(0);
    assert!(state.step_ticks(225).is_empty(), "nothing before 40960 V-timer points");
    assert_eq!(state.step_ticks(1), [tick(A, Status::Poison, 20)]);
    assert_eq!(state.actor(A).unwrap().hp, 620);
    // Ten turns in 2260 ticks (the eleventh is due on the 2476th), the same twenty each time.
    let events = state.step_ticks(2260 - 226);
    let turns = ticks_of(&events);
    assert_eq!(turns.len(), 9);
    assert!(turns.iter().all(|e| *e == tick(A, Status::Poison, 20)));
    assert_eq!(state.actor(A).unwrap().hp, 640 - 200);
    assert!(state.has_status(A, Status::Poison), "Poison does not wear off");
    assert!(ticks_of(&state.step_ticks(215)).is_empty());
    assert_eq!(ticks_of(&state.step_ticks(1)).len(), 1, "the 2476th");

    // Never less than 1, however small the maximum.
    let mut tiny = with_status(hero(), &[Status::Poison]);
    tiny.max_hp = 20;
    tiny.hp = 20;
    let mut state = battle(vec![tiny], vec![brute(5000)], 1);
    state.step_ticks(0);
    assert_eq!(state.step_ticks(226), [tick(A, Status::Poison, 1)]);
}

#[test]
fn poison_can_knock_a_combatant_out_and_that_ends_every_status_and_may_end_the_battle() {
    let weak = |hp: u32| {
        let mut c = with_status(hero(), &[Status::Poison, Status::Barrier]);
        c.max_hp = 64;
        c.hp = hp;
        c
    };
    let mut state = battle(vec![weak(2), weak(40)], vec![brute(5000)], 1);
    state.step_ticks(0);
    assert!(state.step_ticks(225).is_empty());
    assert_eq!(state.step_ticks(1), [tick(A, Status::Poison, 2), BattleEvent::Death { actor: A }, tick(B, Status::Poison, 2)]);
    assert_eq!(state.statuses(A), set(&[Status::Death]));
    assert!(state.actor(A).unwrap().status.is_empty(), "a knockout removes every status");
    assert_eq!((state.actor(A).unwrap().gauge, state.actor(A).unwrap().ready), (0, false));
    assert_eq!(state.actor(B).unwrap().hp, 38, "the other member lost its 2 and goes on");
    assert!(state.has_status(B, Status::Poison) && state.outcome().is_none());

    // The order inside a tick: the actors in order, Poison before the death it causes, and the end of the battle
    // after the last one.
    let mut state = battle(vec![weak(2), weak(2)], vec![brute(5000)], 1);
    state.step_ticks(0);
    let events = state.step_ticks(226);
    assert_eq!(
        events,
        [tick(A, Status::Poison, 2), BattleEvent::Death { actor: A }, tick(B, Status::Poison, 2), BattleEvent::Death { actor: B }, BattleEvent::Defeat]
    );
    assert_eq!(state.outcome(), Some(Outcome::Defeat));
    // And no clock runs once the battle is over.
    assert!(state.step_ticks(500).is_empty());
}

#[test]
fn regen_gives_a_32nd_every_unit_32_times_and_then_wears_off() {
    let mut hurt = with_status(hero(), &[Status::Regen]);
    hurt.hp = 100;
    let mut state = battle(vec![hurt], vec![brute(5000)], 1);
    state.step_ticks(0);
    assert!(ticks_of(&state.step_ticks(90)).is_empty());
    assert_eq!(ticks_of(&state.step_ticks(1)), [tick(A, Status::Regen, 20)]);
    assert_eq!(state.actor(A).unwrap().hp, 120);
    assert_eq!(state.actor(A).unwrap().status.regen_left(), 31);
    // Up to full: 100 + 27 * 20 is 640; the rest of the 32 turns heal nothing and say nothing.
    let events = state.step_ticks(2880 - 91);
    assert_eq!(ticks_of(&events).len(), 26);
    assert_eq!(state.actor(A).unwrap().hp, 640);
    assert!(state.has_status(A, Status::Regen), "the 32nd turn is the 2881st tick");
    assert_eq!(state.actor(A).unwrap().status.regen_left(), 1);
    assert_eq!(state.step_ticks(1), [ended(A, Status::Regen)]);
    assert!(!state.has_status(A, Status::Regen) && state.actor(A).unwrap().status.regen_left() == 0);
    assert!(ticks_of(&state.step_ticks(500)).is_empty());

    // A turn never gives more than the missing HP.
    let mut nearly = with_status(hero(), &[Status::Regen]);
    nearly.hp = 635;
    let mut state = battle(vec![nearly], vec![brute(5000)], 1);
    state.step_ticks(0);
    assert_eq!(ticks_of(&state.step_ticks(91)), [tick(A, Status::Regen, 5)]);
    assert_eq!(state.actor(A).unwrap().hp, 640);
}

#[test]
fn poison_and_regen_run_on_the_clock_of_their_victim() {
    for (pace, poison_at, regen_at) in [(None, 226, 91), (Some(Status::Haste), 113, 46), (Some(Status::Slow), 451, 181)] {
        let pace: Vec<Status> = pace.into_iter().collect();
        let sick = with_status(hero(), &[&[Status::Poison][..], &pace].concat());
        let mut state = battle(vec![sick], vec![brute(5000)], 1);
        state.step_ticks(0);
        let at = tick_of(&mut state, 1000, |e| matches!(e, BattleEvent::StatusTick { status: Status::Poison, .. }));
        assert_eq!(at, Some(poison_at), "Poison with {pace:?}");
        let mut hurt = with_status(hero(), &[&[Status::Regen][..], &pace].concat());
        hurt.hp = 100;
        let mut state = battle(vec![hurt], vec![brute(5000)], 1);
        state.step_ticks(0);
        let at = tick_of(&mut state, 1000, |e| matches!(e, BattleEvent::StatusTick { status: Status::Regen, .. }));
        assert_eq!(at, Some(regen_at), "Regen with {pace:?}");
    }
}

#[test]
fn a_status_that_hurts_runs_before_the_gauges_fill_in_the_same_tick() {
    let mut state = battle(vec![with_status(hero(), &[Status::Poison])], vec![brute(5000)], 1);
    state.step_ticks(0);
    state.step_ticks(225);
    state.actors[0].gauge = GAUGE_MAX - 1;
    let events = state.step_ticks(1);
    assert_eq!(events, [tick(A, Status::Poison, 20), BattleEvent::GaugeReady { actor: A }]);
}

// --- Sleep, Stop and Paralysis hold a turn ------------------------------------------------------------------------

#[test]
fn a_sleeper_holds_its_turn_while_its_gauge_fills_and_a_physical_blow_wakes_it() {
    let mut state = battle(vec![with_status(hero(), &[Status::Sleep])], vec![brute(5000)], 3);
    state.step_ticks(0);
    let events = state.step_ticks(361);
    assert!(events.contains(&BattleEvent::GaugeReady { actor: A }), "the gauge fills in its sleep");
    assert!(state.actor(A).unwrap().ready);
    assert!(state.ready_actors().is_empty() && !state.can_act(A));
    // Every command is refused, and a refusal changes nothing.
    let before = state.clone();
    for command in [Command::Defend, Command::Attack { target: B }, Command::Magic { spell: 27, target: Target::One(B) }] {
        assert_eq!(state.check_command(A, &command), Err(CommandError::CannotAct(A)), "{command:?}");
        assert_eq!(state.submit_command(A, command), Err(CommandError::CannotAct(A)), "{command:?}");
    }
    assert_eq!(state, before);

    // A spell does not wake it, a blow does (and says so after the damage).
    let mut events = Vec::new();
    state.resolve_action(1, &[0], &bolt(), &mut events);
    assert!(matches!(events[..], [BattleEvent::Damage { .. }]), "{events:?}");
    assert!(state.has_status(A, Status::Sleep));
    events.clear();
    state.resolve_action(1, &[0], &blow(), &mut events);
    assert!(matches!(events[..], [BattleEvent::Damage { .. }, BattleEvent::StatusEnded { target: A, status: Status::Sleep }]), "{events:?}");
    assert!(!state.has_status(A, Status::Sleep));
    // It held its turn: the moment it is awake it can use it.
    assert_eq!(state.ready_actors(), [A]);
    assert_eq!(state.check_command(A, &Command::Defend), Ok(()));
}

#[test]
fn a_blow_that_kills_or_does_nothing_does_not_wake_anyone() {
    let mut sleeper = with_status(hero(), &[Status::Sleep, Status::Poison]);
    sleeper.hp = 3;
    let mut state = battle(vec![sleeper, hero()], vec![brute(5000)], 3);
    let mut events = Vec::new();
    state.resolve_action(2, &[0], &blow(), &mut events);
    assert!(matches!(events[..], [BattleEvent::Damage { .. }, BattleEvent::Death { actor: A }]), "{events:?}");
    assert!(state.actor(A).unwrap().status.is_empty(), "a knockout removes the statuses and says nothing of them");
    // A blow that an element nullifies does no damage and so does not wake the sleeper.
    let mut shielded = with_status(brute(5000), &[Status::Sleep]);
    shielded.affinities = vec![ElementAffinity { element: 0, rate: AffinityRate::Nullify }];
    let mut state = battle(vec![hero()], vec![shielded], 3);
    let mut events = Vec::new();
    let null_blow = ActionSpec { element_mask: 1, ..blow() };
    state.resolve_action(0, &[1], &null_blow, &mut events);
    assert_eq!(events, [BattleEvent::Damage { actor: A, target: B, amount: 0, critical: false }]);
    assert!(state.has_status(B, Status::Sleep));
}

#[test]
fn a_sleeping_enemy_waits_with_a_full_gauge_and_acts_the_tick_it_wakes() {
    let sleeper = with_status(brute(5000), &[Status::Sleep]);
    let config = BattleConfig { start: StartMode::EnemiesFirst, ..BattleConfig::default() };
    let mut state = BattleState::with_config(5, vec![hero()], vec![sleeper], config).with_items(items());
    assert_eq!(state.step_ticks(0), [BattleEvent::GaugeReady { actor: B }, inflicted(B, Status::Sleep)]);
    let events = state.step_ticks(120);
    assert!(events.is_empty(), "no attack from a sleeper: {events:?}");
    assert!(state.actor(B).unwrap().ready);
    let mut woken = Vec::new();
    state.resolve_action(0, &[1], &blow(), &mut woken);
    assert_eq!(woken.last(), Some(&ended(B, Status::Sleep)));
    let events = state.step_ticks(1);
    assert!(events.contains(&BattleEvent::Action { actor: B, action: ActionRef::Attack }), "{events:?}");
}

#[test]
fn sleep_runs_on_its_own_clock_so_haste_and_slow_change_how_long_it_lasts() {
    for (statuses, ticks) in [(vec![Status::Sleep], 2701), (vec![Status::Sleep, Status::Haste], 1351), (vec![Status::Sleep, Status::Slow], 5402)] {
        let mut state = battle(vec![with_status(hero(), &statuses)], vec![brute(5000)], 1);
        state.step_ticks(0);
        let at = tick_of(&mut state, 7000, |e| *e == ended(A, Status::Sleep));
        assert_eq!(at, Some(ticks), "{statuses:?}");
        assert!(!state.has_status(A, Status::Sleep));
    }
}

#[test]
fn stop_freezes_the_gauge_and_every_other_clock_for_15_units_of_the_global_timer() {
    let mut state = battle(vec![with_status(hero(), &[Status::Stop, Status::Poison, Status::Barrier])], vec![brute(5000)], 1);
    state.step_ticks(0);
    let events = state.step_ticks(1350);
    assert!(events.is_empty(), "nothing moves while it lasts: {events:?}");
    assert_eq!(state.actor(A).unwrap().gauge, 0, "frozen");
    assert_eq!(state.gauge_increase(A), Some(0));
    assert_eq!(state.actor(A).unwrap().status.points_left(Status::Barrier), Some(BARRIER_DURATION), "so is the Barrier's timer");
    assert!(!state.can_act(A));
    assert_eq!(state.step_ticks(1), [ended(A, Status::Stop)]);
    assert!(state.can_act(A));
    // The clocks start the tick after: the Poison that had not run for 1351 ticks is due 226 ticks later.
    assert!(ticks_of(&state.step_ticks(225)).is_empty());
    assert_eq!(state.step_ticks(1), [tick(A, Status::Poison, 20)]);
    assert_eq!(state.gauge_increase(A), Some(182));
    // Inflicting it again (a timed status that is already there) refills the timer and says nothing.
    let mut again = Vec::new();
    let freeze = ActionSpec::status_spell(StatusEffect::inflict(Status::Stop, 63), 100, 0, false);
    state.resolve_action(1, &[0], &freeze, &mut again);
    assert_eq!(again, [inflicted(A, Status::Stop)]);
    state.step_ticks(600);
    assert!(state.actor(A).unwrap().status.points_left(Status::Stop).unwrap() < STOP_DURATION);
    let mut more = Vec::new();
    state.resolve_action(1, &[0], &freeze, &mut more);
    assert!(more.is_empty(), "{more:?}");
    assert_eq!(state.actor(A).unwrap().status.points_left(Status::Stop), Some(STOP_DURATION));
}

#[test]
fn a_stopped_member_cannot_act_and_a_stopped_enemy_waits_until_it_is_over() {
    let config = BattleConfig { start: StartMode::Preemptive, ..BattleConfig::default() };
    let mut state = BattleState::with_config(2, vec![with_status(hero(), &[Status::Stop])], vec![brute(5000)], config).with_items(items());
    state.step_ticks(0);
    assert!(state.actor(A).unwrap().ready, "its gauge was full when it was stopped");
    assert!(state.ready_actors().is_empty());
    assert_eq!(state.submit_command(A, Command::Defend), Err(CommandError::CannotAct(A)));
    let at = tick_of(&mut state, 2000, |e| *e == ended(A, Status::Stop));
    assert_eq!(at, Some(1351));
    assert_eq!(state.ready_actors(), [A], "and it has its turn again at once");
    assert!(state.submit_command(A, Command::Defend).is_ok());

    let stopped = with_status(brute(5000), &[Status::Stop]);
    let config = BattleConfig { start: StartMode::EnemiesFirst, ..BattleConfig::default() };
    let mut state = BattleState::with_config(2, vec![hero()], vec![stopped], config).with_items(items());
    state.step_ticks(0);
    let at = tick_of(&mut state, 2000, |e| matches!(e, BattleEvent::Action { actor: B, .. }));
    assert_eq!(at, Some(1351), "it acts in the very tick Stop ends (its gauge was full all along)");
}

#[test]
fn paralysis_lasts_8_units_and_fills_no_limit_gauge() {
    let mut state = battle(vec![with_status(hero(), &[Status::Paralysis])], vec![brute(5000)], 1);
    state.step_ticks(0);
    assert!(state.step_ticks(720).is_empty());
    assert_eq!(state.step_ticks(1), [ended(A, Status::Paralysis)]);

    let strong = |statuses: &[Status]| {
        let mut c = with_status(hero(), statuses);
        c.limit_break = Some(LimitBreak::new(128, ActionSpec::limit_strike(48), 140));
        battle(vec![c], vec![brute(5000)], 6)
    };
    let weak_blow = ActionSpec::basic_attack(4, 255, false);
    let hit = |mut state: BattleState| {
        let mut events = Vec::new();
        state.resolve_action(1, &[0], &weak_blow, &mut events);
        let amount = damage_of(&events).expect("it connects");
        (amount, state.limit_gauge(A), events)
    };
    let (plain, plain_gauge, events) = hit(strong(&[]));
    assert_eq!(plain_gauge, limit_gain(plain, 640, 140));
    assert!(plain_gauge > 10 && events.iter().any(|e| matches!(e, BattleEvent::LimitGauge { .. })));
    // Paralysed: nothing. Sadness: half of what the (smaller) blow fills. Fury: twice.
    let (_, paralysed_gauge, events) = hit(strong(&[Status::Paralysis]));
    assert_eq!(paralysed_gauge, 0);
    assert!(!events.iter().any(|e| matches!(e, BattleEvent::LimitGauge { .. })), "no gauge, no event: {events:?}");
    let (sad, sad_gauge, _) = hit(strong(&[Status::Sadness]));
    assert_eq!(sad_gauge, limit_gain(sad, 640, 140) / 2);
    assert_eq!(sad, plain * 7 / 10);
    let (furious, furious_gauge, _) = hit(strong(&[Status::Fury]));
    assert_eq!(furious_gauge, limit_gain(furious, 640, 140) * 2);
    assert_eq!(furious, plain);
}

// --- Silence ------------------------------------------------------------------------------------------------------

#[test]
fn silence_closes_the_magic_command_and_nothing_else() {
    let mut state = battle(vec![with_status(hero(), &[Status::Silence]), hero()], vec![brute(5000)], 1);
    wait_ready(&mut state, A);
    wait_ready(&mut state, B);
    assert!(state.is_silenced(A) && !state.is_silenced(B));
    let fire = Command::Magic { spell: 27, target: Target::One(C) };
    assert_eq!(state.check_command(A, &fire), Err(CommandError::Silenced(A)));
    // Before the checks of the spell itself: an unknown spell, one it has no MP for and a bad target are all silence.
    assert_eq!(state.check_command(A, &Command::Magic { spell: 99, target: Target::One(C) }), Err(CommandError::Silenced(A)));
    state.actors[0].mp = 0;
    assert_eq!(state.check_command(A, &Command::Magic { spell: 0, target: Target::AllEnemies }), Err(CommandError::Silenced(A)));
    state.actors[0].mp = 100;
    let before = state.clone();
    assert_eq!(state.submit_command(A, fire), Err(CommandError::Silenced(A)));
    assert_eq!(state, before);
    // Attack, items and the guard are open, and the one that is not silenced casts.
    assert_eq!(state.check_command(A, &Command::Attack { target: C }), Ok(()));
    assert_eq!(state.check_command(A, &Command::Item { item: 0, target: Target::One(A) }), Ok(()));
    assert_eq!(state.check_command(A, &Command::Defend), Ok(()));
    assert_eq!(state.check_command(B, &Command::Magic { spell: 27, target: Target::One(C) }), Ok(()));
    // The menu's question (`spells_for`) still lists the spells: it is the state that says who is silenced.
    assert_eq!(state.spells_for(A).len(), 2);
}

#[test]
fn an_echo_screen_gives_the_voice_back() {
    let mut state = battle(vec![with_status(hero(), &[Status::Silence])], vec![brute(5000)], 1);
    wait_ready(&mut state, A);
    let events = state.submit_command(A, Command::Item { item: 9, target: Target::One(A) }).unwrap();
    assert_eq!(events, [BattleEvent::Action { actor: A, action: ActionRef::Item(9) }, ended(A, Status::Silence)]);
    assert!(!state.is_silenced(A));
    wait_ready(&mut state, A);
    assert_eq!(state.check_command(A, &Command::Magic { spell: 27, target: Target::One(B) }), Ok(()));
}

// --- Haste, Slow and the other pairs ------------------------------------------------------------------------------

#[test]
fn haste_doubles_the_gauge_and_slow_halves_it() {
    for (statuses, increase, fills_at) in [(vec![], 182, 361), (vec![Status::Haste], 364, 181), (vec![Status::Slow], 91, 721)] {
        let mut state = battle(vec![with_status(hero(), &statuses)], vec![brute(5000)], 1);
        state.step_ticks(0);
        assert_eq!(state.gauge_increase(A), Some(increase), "{statuses:?}");
        let at = tick_of(&mut state, 800, |e| *e == BattleEvent::GaugeReady { actor: A });
        assert_eq!(at, Some(fills_at), "{statuses:?}");
    }
}

#[test]
fn haste_and_slow_cancel_each_other_and_so_do_sadness_and_fury() {
    let curse = |status| ActionSpec::status_spell(StatusEffect::inflict(status, 63), 100, 0, false);
    let mut state = battle(vec![hero()], vec![with_status(brute(5000), &[Status::Haste, Status::Sadness])], 7);
    let mut events = Vec::new();
    state.resolve_action(0, &[1], &curse(Status::Slow), &mut events);
    assert_eq!(events, [ended(B, Status::Haste), inflicted(B, Status::Slow)]);
    events.clear();
    state.resolve_action(0, &[1], &curse(Status::Fury), &mut events);
    assert_eq!(events, [ended(B, Status::Sadness), inflicted(B, Status::Fury)]);
    assert_eq!(state.statuses(B), set(&[Status::Slow, Status::Fury]));
    // The same one again changes nothing and says nothing.
    events.clear();
    state.resolve_action(0, &[1], &curse(Status::Slow), &mut events);
    assert!(events.is_empty());
}

#[test]
fn an_enemy_that_cannot_be_hasted_cannot_be_slowed_either() {
    let curse = |status| ActionSpec::status_spell(StatusEffect::inflict(status, 63), 100, 0, false);
    for immune in [Status::Haste, Status::Slow] {
        let mut proud = brute(5000);
        proud.status_immune = set(&[immune]);
        let mut state = battle(vec![hero()], vec![proud], 7);
        let mut events = Vec::new();
        state.resolve_action(0, &[1], &curse(Status::Slow), &mut events);
        state.resolve_action(0, &[1], &curse(Status::Haste), &mut events);
        // The hit rolls and the chance are drawn all the same, there is just no effect to announce.
        assert!(events.is_empty(), "{immune:?}: {events:?}");
        assert_eq!(state.statuses(B), StatusSet::EMPTY);
    }
    let mut state = battle(vec![hero()], vec![brute(5000)], 7);
    let mut events = Vec::new();
    state.resolve_action(0, &[1], &curse(Status::Slow), &mut events);
    assert_eq!(events, [inflicted(B, Status::Slow)]);
}

#[test]
fn an_enemy_record_is_immune_to_the_statuses_whose_bit_is_clear() {
    // The record's mask says what AFFECTS the enemy: all ones is "affected by everything".
    let bin = SceneBin::parse(&pack_scene_bin(&[gzip(&demo_scene(0).raw)], 4)).unwrap();
    let record = bin.scene(0).unwrap().enemies[0].clone().unwrap();
    let open = EnemyRecord { status_immunities: 0xFFFF_FFFF, ..record.clone() };
    assert_eq!(Combatant::from_enemy(&open, None).status_immune, StatusSet::EMPTY);
    let closed = EnemyRecord { status_immunities: !(Status::Sleep.bit() | Status::Poison.bit()), ..record };
    assert_eq!(Combatant::from_enemy(&closed, None).status_immune, set(&[Status::Sleep, Status::Poison]));
    assert_eq!(hero().status_immune, StatusSet::EMPTY, "armor protections are not modelled");
}

// --- what a blow does ---------------------------------------------------------------------------------------------

/// What `spec` does to a hero that has `statuses` when the brute uses it with `seed`: the damage, and the generator
/// afterwards.
fn damage_to_hero(statuses: &[Status], spec: &ActionSpec, seed: u64) -> (u32, Rng) {
    let mut state = battle(vec![with_status(hero(), statuses)], vec![brute(5000)], seed);
    let mut events = Vec::new();
    state.resolve_action(1, &[0], spec, &mut events);
    (damage_of(&events).expect("it connects"), state.rng)
}

#[test]
fn barrier_mbarrier_and_sadness_take_their_share_off_a_blow_and_change_no_draw() {
    let seed = 12;
    let (p, rng_p) = damage_to_hero(&[], &blow(), seed);
    let (m, rng_m) = damage_to_hero(&[], &bolt(), seed);
    assert!(p > 40 && m > 40, "numbers worth halving: {p} {m}");
    for (statuses, physical, magical) in [
        (vec![Status::Barrier], p / 2, m),
        (vec![Status::MBarrier], p, m / 2),
        (vec![Status::Barrier, Status::MBarrier], p / 2, m / 2),
        (vec![Status::Sadness], p * 7 / 10, m * 7 / 10),
        (vec![Status::Barrier, Status::Sadness], p / 2 * 7 / 10, m * 7 / 10),
        (vec![Status::Fury, Status::Haste, Status::Regen, Status::Silence, Status::Darkness], p, m),
    ] {
        let (got_p, got_rng_p) = damage_to_hero(&statuses, &blow(), seed);
        let (got_m, got_rng_m) = damage_to_hero(&statuses, &bolt(), seed);
        assert_eq!((got_p, got_m), (physical, magical), "{statuses:?}");
        assert_eq!((got_rng_p, got_rng_m), (rng_p.clone(), rng_m.clone()), "{statuses:?}: the draws are the same");
    }
    // Defend and Barrier both halve a physical blow, one after the other.
    let guarded = |statuses: &[Status]| {
        let mut state = battle(vec![with_status(hero(), statuses)], vec![brute(5000)], seed);
        state.actors[0].defending = true;
        let mut events = Vec::new();
        state.resolve_action(1, &[0], &blow(), &mut events);
        damage_of(&events).unwrap()
    };
    assert!(guarded(&[]) < p * 6 / 10, "Defend alone halves it");
    assert_eq!(guarded(&[Status::Barrier]), guarded(&[]) / 2);
}

/// The roll to hit of a physical action on the first of `seed`'s draws: the luck roll comes first, then this.
fn physical_miss_roll(seed: u64) -> u32 {
    let mut replay = Rng::new(seed);
    replay.range_inclusive(0, 99);
    percent_roll(replay.next_u16())
}

/// The roll to hit of a magical action (no luck roll).
fn magic_miss_roll(seed: u64) -> u32 {
    percent_roll(Rng::new(seed).next_u16())
}

/// Whether `spec` connects when `attacker` (a party member or an enemy) uses it on a plain target with `seed`.
fn connects(attacker: Combatant, party_member: bool, spec: &ActionSpec, seed: u64) -> bool {
    let target = Combatant::new("Target", 10, 5000);
    let (party, enemies, from, to) = if party_member { (vec![attacker], vec![target], 0, 1) } else { (vec![target], vec![attacker], 1, 0) };
    let mut state = battle(party, enemies, seed);
    let mut events = Vec::new();
    state.resolve_action(from, &[to], spec, &mut events);
    !events.contains(&BattleEvent::Miss { actor: ActorId(from as u8), target: ActorId(to as u8) })
}

#[test]
fn fury_takes_30_percent_off_the_chance_to_hit_and_darkness_halves_a_members_attack_command_only() {
    let seed = (0u64..).find(|s| physical_miss_roll(*s) >= 10 && magic_miss_roll(*s) >= 10).unwrap();
    let (rp, rm) = (physical_miss_roll(seed), magic_miss_roll(seed));
    // A Dex of 0, no Luck and no evade on either side: the chance to hit is the accuracy of the action, which
    // is set to the very roll, so that the smallest cut makes it miss.
    let striker = |statuses: &[Status]| with_status(Combatant::new("Striker", 10, 640), statuses);
    let attack = ActionSpec::basic_attack(16, rp, false);
    let spell = ActionSpec::damage_spell(8, rm, element::FIRE, 0);
    let curse = ActionSpec::status_spell(StatusEffect::inflict(Status::Sleep, 63), rm, 0, false);
    assert!(connects(striker(&[]), true, &attack, seed) && connects(striker(&[]), false, &attack, seed));
    assert!(connects(striker(&[]), true, &spell, seed) && connects(striker(&[]), true, &curse, seed));
    // Darkness: the Attack command of a party member, and nothing else.
    assert!(!connects(striker(&[Status::Darkness]), true, &attack, seed), "a darkened member misses");
    assert!(connects(striker(&[Status::Darkness]), false, &attack, seed), "a darkened enemy does not (the game's own rule)");
    assert!(connects(striker(&[Status::Darkness]), true, &spell, seed), "nor does a darkened caster");
    assert!(connects(striker(&[Status::Darkness]), true, &curse, seed));
    // Fury: every attack of whoever has it, whichever side.
    assert!(!connects(striker(&[Status::Fury]), true, &attack, seed));
    assert!(!connects(striker(&[Status::Fury]), false, &attack, seed));
    assert!(!connects(striker(&[Status::Fury]), true, &spell, seed));
    assert!(!connects(striker(&[Status::Fury]), true, &curse, seed));
    // The others do not touch the chance to hit.
    assert!(connects(striker(&[Status::Sadness, Status::Haste, Status::Barrier, Status::Silence]), true, &attack, seed));
}

// --- what an action with a status effect draws --------------------------------------------------------------------

/// The status rolls of an action that deals damage to `targets` targets: for each one the roll to hit and the
/// variance come first, then this.
fn status_rolls(seed: u64, targets: usize) -> Vec<u32> {
    let mut replay = Rng::new(seed);
    (0..targets)
        .map(|_| {
            replay.next_u16();
            replay.range_inclusive(0, 255);
            replay.below(63)
        })
        .collect()
}

fn poisoning(chance: u8) -> ActionSpec {
    ActionSpec::damage_spell(8, 100, element::FIRE, 0).with_status(StatusEffect::inflict(Status::Poison, chance))
}

#[test]
fn a_blow_with_a_status_rolls_it_after_its_damage_and_inflicts_it_when_the_roll_is_below_the_chance() {
    // One target: the roll to hit, the variance, then the roll of the status; 63 of 63 never fails and 0 never works.
    let seed = 21;
    for (chance, expect) in [(63, true), (0, false)] {
        let mut state = battle(vec![hero()], vec![brute(5000)], seed);
        let mut events = Vec::new();
        state.resolve_action(0, &[1], &poisoning(chance), &mut events);
        let mut replay = Rng::new(seed);
        replay.next_u16();
        replay.range_inclusive(0, 255);
        replay.below(63);
        assert_eq!(state.rng, replay, "chance {chance}");
        let expected: &[BattleEvent] = if expect { &[inflicted(B, Status::Poison)] } else { &[] };
        assert_eq!(&events[1..], expected, "chance {chance}: {events:?}");
        assert!(matches!(events[0], BattleEvent::Damage { target: B, .. }));
        assert_eq!(state.has_status(B, Status::Poison), expect);
    }
    // A chance in between is a comparison with that draw.
    let below = (0u64..).find(|s| status_rolls(*s, 1)[0] < 21).unwrap();
    let above = (0u64..).find(|s| status_rolls(*s, 1)[0] >= 21).unwrap();
    for (seed, expect) in [(below, true), (above, false)] {
        let mut state = battle(vec![hero()], vec![brute(5000)], seed);
        let mut events = Vec::new();
        state.resolve_action(0, &[1], &poisoning(21), &mut events);
        assert_eq!(state.has_status(B, Status::Poison), expect, "seed {seed}");
    }
}

#[test]
fn a_status_that_splits_with_its_blow_is_cut_to_two_thirds_for_each_of_several_targets() {
    // 63 of 63 becomes 42: a first roll in 42..63 fails with two targets and works against one.
    let seed = (0u64..).find(|s| (42..63).contains(&status_rolls(*s, 2)[0]) && status_rolls(*s, 2)[1] < 42).unwrap();
    let mut two = battle(vec![hero()], vec![brute(5000), brute(5000)], seed);
    let mut events = Vec::new();
    two.resolve_action(0, &[1, 2], &poisoning(63), &mut events);
    let kinds: Vec<&str> = events.iter().map(|e| match e {
        BattleEvent::Damage { .. } => "damage",
        BattleEvent::StatusInflicted { .. } => "inflicted",
        other => panic!("{other:?}"),
    }).collect();
    assert_eq!(kinds, ["damage", "damage", "inflicted"], "the first target was spared, the second was not");
    assert_eq!(events[2], inflicted(C, Status::Poison));
    // Every target's draws, in actor order: the roll to hit, the variance, the status roll.
    let mut replay = Rng::new(seed);
    for _ in 0..2 {
        replay.next_u16();
        replay.range_inclusive(0, 255);
        replay.below(63);
    }
    assert_eq!(two.rng, replay);
    let mut one = battle(vec![hero()], vec![brute(5000)], seed);
    let mut events = Vec::new();
    one.resolve_action(0, &[1], &poisoning(63), &mut events);
    assert_eq!(events[1], inflicted(B, Status::Poison), "the same roll, one target: no cut");
    // A spell that never splits (a forced whole side, a fraction of HP) is not cut: only Formula::Spell with a choice
    // of one or a side splits.
    let forced = ActionSpec { aim: Aim::Side, ..poisoning(63) };
    assert!(!forced.splits());
    let mut state = battle(vec![hero()], vec![brute(5000), brute(5000)], seed);
    let mut events = Vec::new();
    state.resolve_action(0, &[1, 2], &forced, &mut events);
    assert_eq!(events.iter().filter(|e| matches!(e, BattleEvent::StatusInflicted { .. })).count(), 2);
}

#[test]
fn a_target_that_is_missed_or_knocked_out_draws_no_status_roll() {
    let seed = 33;
    // Knocked out, with another enemy still standing: the first target draws its hit and variance only.
    let mut state = battle(vec![hero()], vec![brute(1), brute(5000)], seed);
    let mut events = Vec::new();
    state.resolve_action(0, &[1, 2], &poisoning(63), &mut events);
    let mut replay = Rng::new(seed);
    replay.next_u16();
    replay.range_inclusive(0, 255);
    replay.next_u16();
    replay.range_inclusive(0, 255);
    let roll = replay.below(63);
    assert_eq!(state.rng, replay);
    assert!(matches!(events[0], BattleEvent::Damage { target: B, .. }) && events[1] == BattleEvent::Death { actor: B }, "{events:?}");
    assert_eq!(events.len(), if roll < 42 { 4 } else { 3 }, "{events:?}");
    assert_eq!(events.contains(&inflicted(C, Status::Poison)), roll < 42);
    assert!(state.actor(B).unwrap().status.is_empty());
    // The last enemy knocked out: the battle is over, nothing more is rolled.
    let mut state = battle(vec![hero()], vec![brute(1)], seed);
    let mut events = Vec::new();
    state.resolve_action(0, &[1], &poisoning(63), &mut events);
    let mut replay = Rng::new(seed);
    replay.next_u16();
    replay.range_inclusive(0, 255);
    assert_eq!(state.rng, replay);
    assert!(matches!(events[..], [BattleEvent::Damage { .. }, BattleEvent::Death { actor: B }, BattleEvent::Victory]), "{events:?}");
    // Missed: the roll to hit and nothing else.
    let mut state = battle(vec![hero()], vec![brute(5000)], seed);
    let mut events = Vec::new();
    let miss = ActionSpec { accuracy: 0, ..poisoning(63) };
    state.resolve_action(0, &[1], &miss, &mut events);
    assert_eq!(events, [BattleEvent::Miss { actor: A, target: B }]);
    let mut replay = Rng::new(seed);
    replay.next_u16();
    assert_eq!(state.rng, replay);
}

#[test]
fn a_status_only_action_rolls_to_hit_a_curse_but_not_a_buff_or_an_item() {
    let seed = 8;
    let sleep = StatusEffect::inflict(Status::Sleep, 63);
    let haste = StatusEffect::inflict(Status::Haste, 63);
    // A curse at an enemy: the roll to hit like a spell's, then the roll of the status.
    let mut state = battle(vec![hero()], vec![brute(5000)], seed);
    let mut events = Vec::new();
    state.resolve_action(0, &[1], &ActionSpec::status_spell(sleep, 100, 0, false), &mut events);
    assert_eq!(events, [inflicted(B, Status::Sleep)]);
    let mut replay = Rng::new(seed);
    replay.next_u16();
    replay.below(63);
    assert_eq!(state.rng, replay);
    // A curse that misses draws the roll to hit only.
    let mut state = battle(vec![hero()], vec![brute(5000)], seed);
    let mut events = Vec::new();
    state.resolve_action(0, &[1], &ActionSpec::status_spell(sleep, 0, 0, false), &mut events);
    assert_eq!(events, [BattleEvent::Miss { actor: A, target: B }]);
    let mut replay = Rng::new(seed);
    replay.next_u16();
    assert_eq!(state.rng, replay);
    // A curse can be a physical one too: the luck roll and the roll to hit first.
    let mut state = battle(vec![hero()], vec![brute(5000)], seed);
    let physical = ActionSpec { kind: ActionKind::Physical, ..ActionSpec::status_spell(sleep, 255, 0, false) };
    let mut events = Vec::new();
    state.resolve_action(0, &[1], &physical, &mut events);
    assert_eq!(events, [inflicted(B, Status::Sleep)]);
    let mut replay = Rng::new(seed);
    replay.range_inclusive(0, 99);
    replay.next_u16();
    replay.below(63);
    assert_eq!(state.rng, replay);
    // A buff, a cure and an item are never missed: the roll of the status only.
    for spec in [
        ActionSpec::status_spell(haste, 100, 0, true),
        ActionSpec::status_item(haste, true),
        ActionSpec::status_item(sleep, false),
    ] {
        let mut state = battle(vec![hero(), hero()], vec![brute(5000)], seed);
        let target = if spec.on_allies { 1 } else { 2 };
        let mut events = Vec::new();
        state.resolve_action(0, &[target], &spec, &mut events);
        assert_eq!(events.len(), 1, "{events:?}");
        let mut replay = Rng::new(seed);
        replay.below(63);
        assert_eq!(state.rng, replay, "{spec:?}");
    }
}

#[test]
fn an_immune_target_costs_its_status_roll_all_the_same_and_nothing_is_announced() {
    let seed = 40;
    let mut proof = brute(5000);
    proof.status_immune = set(&[Status::Poison]);
    let mut state = battle(vec![hero()], vec![proof], seed);
    let mut events = Vec::new();
    state.resolve_action(0, &[1], &poisoning(63), &mut events);
    assert!(matches!(events[..], [BattleEvent::Damage { .. }]), "{events:?}");
    let mut replay = Rng::new(seed);
    replay.next_u16();
    replay.range_inclusive(0, 255);
    replay.below(63);
    assert_eq!(state.rng, replay);
    assert_eq!(state.statuses(B), StatusSet::EMPTY);
    // A status that is already there is not announced again either.
    let mut state = battle(vec![hero()], vec![with_status(brute(5000), &[Status::Poison])], seed);
    let mut events = Vec::new();
    state.resolve_action(0, &[1], &poisoning(63), &mut events);
    assert!(matches!(events[..], [BattleEvent::Damage { .. }]), "{events:?}");
}

#[test]
fn a_toggle_cures_what_the_target_has_and_inflicts_what_it_has_not() {
    let toggle = StatusEffect { set: set(&[Status::Poison, Status::Haste]), chance: 63, mode: StatusMode::Toggle };
    let spec = ActionSpec::status_spell(toggle, 100, 0, true);
    let mut state = battle(vec![hero(), with_status(hero(), &[Status::Poison])], vec![brute(5000)], 3);
    let mut events = Vec::new();
    state.resolve_action(0, &[1], &spec, &mut events);
    assert_eq!(events, [ended(B, Status::Poison), inflicted(B, Status::Haste)]);
    events.clear();
    state.resolve_action(0, &[1], &spec, &mut events);
    assert_eq!(events, [inflicted(B, Status::Poison), ended(B, Status::Haste)]);
    // A cure of what the target has not says nothing.
    let antidote = ActionSpec::status_item(StatusEffect::cure(Status::Poison, 63), true);
    let mut state = battle(vec![hero()], vec![brute(5000)], 3);
    let mut events = Vec::new();
    state.resolve_action(0, &[0], &antidote, &mut events);
    assert!(events.is_empty());
}

#[test]
fn an_enemy_attack_with_a_status_inflicts_it_after_its_damage() {
    let seed = 17;
    let mut stinger = brute(5000);
    stinger.actions = vec![EnemyAction { id: 7, spec: ActionSpec::basic_attack(4, 255, true).with_status(StatusEffect::inflict(Status::Silence, 63)) }];
    let config = BattleConfig { start: StartMode::EnemiesFirst, ..BattleConfig::default() };
    let mut state = BattleState::with_config(seed, vec![hero()], vec![stinger], config).with_items(items());
    let events = state.step_ticks(1);
    let kinds: Vec<&str> = events.iter().map(|e| match e {
        BattleEvent::GaugeReady { .. } => "ready",
        BattleEvent::Action { action: ActionRef::EnemyAttack(7), .. } => "action",
        BattleEvent::Damage { .. } => "damage",
        BattleEvent::StatusInflicted { status: Status::Silence, .. } => "silence",
        other => panic!("{other:?}"),
    }).collect();
    assert_eq!(kinds, ["ready", "action", "damage", "silence"]);
    // The turn's draws: the target (of one), then the blow's (luck, hit, critical, variance), then the status.
    let mut replay = Rng::new(seed);
    replay.below(1);
    replay.range_inclusive(0, 99);
    replay.next_u16();
    replay.next_u16();
    replay.range_inclusive(0, 255);
    replay.below(63);
    assert_eq!(state.rng, replay);
    // And the member that was struck cannot cast any more.
    assert!(state.is_silenced(A));
    wait_ready(&mut state, A);
    assert_eq!(state.check_command(A, &Command::Magic { spell: 27, target: Target::One(B) }), Err(CommandError::Silenced(A)));
}

#[test]
fn the_random_choice_of_an_enemy_attack_is_not_widened_by_statuses() {
    // An enemy of the scene keeps the damaging attacks it had and gains no status-only one: the draws of a battle that
    // has no status in it are what they were.
    let bin = SceneBin::parse(&pack_scene_bin(&[gzip(&demo_scene(0).raw)], 4)).unwrap();
    let enemies = enemies_for_formation(bin.scene(0).unwrap(), 0);
    assert_eq!(enemies[0].actions.iter().map(|a| a.id).collect::<Vec<_>>(), [0x0302, 0x0301]);
    assert!(enemies.iter().flat_map(|e| &e.actions).all(|a| a.spec.status.is_none() && a.spec.formula != Formula::None));
}

// --- items and spells that only change statuses -------------------------------------------------------------------

#[test]
fn curing_items_work_through_the_item_command_and_say_what_they_cured() {
    let sick = with_status(hero(), &[Status::Sleep, Status::Poison, Status::Silence, Status::Darkness, Status::Regen]);
    let mut state = battle(vec![hero(), sick], vec![brute(5000)], 9);
    wait_ready(&mut state, A);
    // An Antidote on someone who has no Poison is used and cures nothing.
    let events = state.submit_command(A, Command::Item { item: 8, target: Target::One(A) }).unwrap();
    assert_eq!(events, [BattleEvent::Action { actor: A, action: ActionRef::Item(8) }]);
    assert!(!state.actor(A).unwrap().ready, "the turn is spent all the same");
    wait_ready(&mut state, A);
    let events = state.submit_command(A, Command::Item { item: 8, target: Target::One(B) }).unwrap();
    assert_eq!(events, [BattleEvent::Action { actor: A, action: ActionRef::Item(8) }, ended(B, Status::Poison)]);
    assert_eq!(state.statuses(B), set(&[Status::Sleep, Status::Silence, Status::Darkness, Status::Regen]));
    // A Remedy cures all it lists, in bit order, and leaves a buff alone.
    wait_ready(&mut state, A);
    let events = state.submit_command(A, Command::Item { item: 11, target: Target::One(B) }).unwrap();
    assert_eq!(
        events,
        [
            BattleEvent::Action { actor: A, action: ActionRef::Item(11) },
            ended(B, Status::Sleep),
            ended(B, Status::Silence),
            ended(B, Status::Darkness)
        ]
    );
    assert_eq!(state.statuses(B), set(&[Status::Regen]));
}

#[test]
fn a_cure_for_the_whole_party_cures_each_living_member_in_order() {
    let mut fallen = with_status(hero(), &[Status::Poison]);
    fallen.hp = 0;
    let party = vec![
        with_status(hero(), &[Status::Poison]),
        with_status(hero(), &[Status::Silence, Status::Darkness]),
        fallen,
        with_status(hero(), &[Status::Haste]),
    ];
    let mut state = battle(party, vec![brute(5000)], 9);
    wait_ready(&mut state, A);
    let events = state.submit_command(A, Command::Item { item: 12, target: Target::AllAllies }).unwrap();
    assert_eq!(
        events,
        [
            BattleEvent::Action { actor: A, action: ActionRef::Item(12) },
            ended(A, Status::Poison),
            ended(B, Status::Silence),
            ended(B, Status::Darkness)
        ]
    );
    // One at a time, a whole-party item is refused (it only hits a whole side) and so is the wrong side.
    wait_ready(&mut state, A);
    assert_eq!(state.check_command(A, &Command::Item { item: 12, target: Target::One(B) }), Err(CommandError::InvalidReach(Target::One(B))));
    assert_eq!(state.check_command(A, &Command::Item { item: 12, target: Target::AllEnemies }), Err(CommandError::InvalidReach(Target::AllEnemies)));
}

#[test]
fn an_item_that_only_changes_statuses_says_which_side_it_aims_at() {
    let state = battle(vec![hero(), hero()], vec![brute(5000)], 1);
    let (antidote, remedy_all, speed, dazers, potion) = (
        state.item_info(8).unwrap(),
        state.item_info(12).unwrap(),
        state.item_info(13).unwrap(),
        state.item_info(14).unwrap(),
        state.item_info(0).unwrap(),
    );
    assert_eq!((antidote.kind, antidote.targets_allies, antidote.revives, antidote.aim), (ItemKind::Status, true, false, Aim::One));
    assert_eq!((remedy_all.kind, remedy_all.targets_allies, remedy_all.aim), (ItemKind::Status, true, Aim::Side));
    assert_eq!((speed.kind, speed.targets_allies), (ItemKind::Status, true));
    assert_eq!((dazers.kind, dazers.targets_allies), (ItemKind::Status, false), "a curse is thrown at the enemies");
    assert_eq!(potion.kind, ItemKind::RestoreHp);
    // The Dazers goes at an enemy, a Speed Drink at a friend, and not the other way round.
    let mut state = battle(vec![hero(), hero()], vec![brute(5000)], 4);
    wait_ready(&mut state, A);
    assert_eq!(state.check_command(A, &Command::Item { item: 14, target: Target::One(B) }), Err(CommandError::InvalidTarget(B)));
    assert_eq!(state.check_command(A, &Command::Item { item: 13, target: Target::One(C) }), Err(CommandError::InvalidTarget(C)));
    assert_eq!(state.check_command(A, &Command::Item { item: 14, target: Target::AllEnemies }), Err(CommandError::InvalidReach(Target::AllEnemies)));
    let events = state.submit_command(A, Command::Item { item: 14, target: Target::One(C) }).unwrap();
    assert_eq!(events, [BattleEvent::Action { actor: A, action: ActionRef::Item(14) }, inflicted(C, Status::Sleep)]);
    wait_ready(&mut state, B);
    let events = state.submit_command(B, Command::Item { item: 13, target: Target::One(A) }).unwrap();
    assert_eq!(events, [BattleEvent::Action { actor: B, action: ActionRef::Item(13) }, inflicted(A, Status::Haste)]);
    assert_eq!(state.gauge_increase(A), Some(364));
}

#[test]
fn a_status_spell_aims_by_the_bit_of_its_record_and_a_whole_side_costs_an_all_use() {
    let sleep = Spell::new(54, ActionSpec::status_spell(StatusEffect::inflict(Status::Sleep, 63), 100, 6, false));
    let haste = Spell::new(55, ActionSpec::status_spell(StatusEffect::inflict(Status::Haste, 63), 100, 10, true));
    let (sleep_info, haste_info) = (sleep.info(), haste.info());
    assert_eq!((sleep_info.targets_allies, sleep_info.aim), (false, Aim::OneOrSide));
    assert_eq!((haste_info.targets_allies, haste_info.aim), (true, Aim::OneOrSide));
    let mut caster = hero();
    caster.spells = vec![sleep, haste];
    caster.all_left = 1;
    let mut state = battle(vec![caster, hero()], vec![brute(5000), brute(5000)], 12);
    wait_ready(&mut state, A);
    let at = |id: u8| Target::One(ActorId(id));
    assert_eq!(state.check_command(A, &Command::Magic { spell: 54, target: at(1) }), Err(CommandError::InvalidTarget(B)));
    assert_eq!(state.check_command(A, &Command::Magic { spell: 55, target: at(2) }), Err(CommandError::InvalidTarget(C)));
    assert_eq!(state.check_command(A, &Command::Magic { spell: 54, target: Target::AllAllies }), Err(CommandError::InvalidReach(Target::AllAllies)));
    let events = state.submit_command(A, Command::Magic { spell: 54, target: Target::AllEnemies }).unwrap();
    assert_eq!(
        events,
        [
            BattleEvent::Action { actor: A, action: ActionRef::Magic(54) },
            BattleEvent::MpSpent { actor: A, amount: 6 },
            inflicted(C, Status::Sleep),
            inflicted(ActorId(3), Status::Sleep)
        ]
    );
    assert_eq!(state.all_left(A), 0, "a whole side with a spell that can be aimed at one costs an All use");
    wait_ready(&mut state, A);
    assert_eq!(
        state.check_command(A, &Command::Magic { spell: 55, target: Target::AllAllies }),
        Err(CommandError::NoAllLeft(A))
    );
    let events = state.submit_command(A, Command::Magic { spell: 55, target: at(1) }).unwrap();
    assert_eq!(events[2], inflicted(B, Status::Haste));
}

#[test]
fn the_wire_words_of_the_status_events() {
    for (event, json) in [
        (inflicted(B, Status::Poison), r#"{"StatusInflicted":{"target":1,"status":"Poison"}}"#),
        (ended(A, Status::MBarrier), r#"{"StatusEnded":{"target":0,"status":"MBarrier"}}"#),
        (tick(C, Status::Regen, 20), r#"{"StatusTick":{"target":2,"status":"Regen","amount":20}}"#),
    ] {
        assert_eq!(serde_json::to_string(&event).unwrap(), json);
        assert_eq!(serde_json::from_str::<BattleEvent>(json).unwrap(), event);
    }
    assert!(serde_json::from_str::<BattleEvent>(r#"{"StatusEnded":{"target":0,"status":"Nonsense"}}"#).is_err());
}

// --- whole battles ------------------------------------------------------------------------------------------------

fn lcg(state: &mut u64) -> u64 {
    *state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
    *state >> 33
}

/// An enemy that strikes with each of `attacks` in turn, each inflicting its status at its chance.
fn striker(name: &str, attacks: &[(u16, Status, u8)]) -> Combatant {
    let mut c = Combatant::new(name, 12, 2500);
    c.attack = 40;
    c.magic = 20;
    c.defense = 12;
    c.magic_defense = 12;
    c.dex = 70;
    c.actions = attacks
        .iter()
        .map(|&(id, status, chance)| EnemyAction { id, spec: ActionSpec::basic_attack(10, 120, true).with_status(StatusEffect::inflict(status, chance)) })
        .collect();
    c
}

/// Four heroes with statuses of their own and three enemies that inflict most of the ones the rules play.
fn status_battle(seed: u64) -> BattleState {
    let first = with_status(hero(), &[Status::Poison, Status::Haste]);
    let mut second = with_status(hero(), &[Status::Regen]);
    second.hp = 300;
    let third = with_status(hero(), &[Status::Silence, Status::Darkness]);
    let mut fourth = hero();
    fourth.all_left = 2;
    let enemies = vec![
        striker("Wasp", &[(1, Status::Poison, 30), (2, Status::Sleep, 25)]),
        striker("Toad", &[(3, Status::Silence, 30), (4, Status::Slow, 30), (5, Status::Stop, 20), (6, Status::Paralysis, 15)]),
        striker("Imp", &[(7, Status::Darkness, 30), (8, Status::Sadness, 30), (9, Status::Fury, 30)]),
    ];
    BattleState::with_config(seed, vec![first, second, third, fourth], enemies, zero_start()).with_items(items())
}

/// Every command a member might try, the valid and the not: a mix of targets, sides and items.
fn candidates(state: &BattleState, actor: ActorId) -> Vec<Command> {
    let mut list = vec![
        Command::Defend,
        Command::Attack { target: actor },
        Command::Item { item: 99, target: Target::One(actor) },
        Command::Magic { spell: 99, target: Target::AllEnemies },
        Command::Magic { spell: 27, target: Target::AllEnemies },
        Command::Magic { spell: 0, target: Target::AllAllies },
        Command::Item { item: 12, target: Target::AllAllies },
        Command::Item { item: 12, target: Target::AllEnemies },
    ];
    for id in state.party_ids().into_iter().chain(state.enemy_ids()) {
        list.push(Command::Attack { target: id });
        list.push(Command::Magic { spell: 27, target: Target::One(id) });
        list.push(Command::Magic { spell: 0, target: Target::One(id) });
        for item in [0, 8, 9, 10, 11, 13, 14] {
            list.push(Command::Item { item, target: Target::One(id) });
        }
    }
    list
}

/// What must hold of any state of a battle, however it got there.
fn assert_sane(state: &BattleState) {
    for (i, c) in state.actors().iter().enumerate() {
        let id = ActorId(i as u8);
        assert!(c.hp <= c.max_hp && c.mp <= c.max_mp, "{id:?}");
        assert_eq!(c.status.set().intersection(MODELLED), c.status.set(), "only the statuses the rules play are kept: {id:?}");
        assert!(c.status.set().bits() & (Status::Death.bit() | Status::NearDeath.bit()) == 0, "neither is stored");
        if !c.is_alive() {
            assert!(c.status.is_empty() && !c.ready && c.gauge == 0 && c.limit == 0, "a knockout removes every status: {id:?}");
            assert_eq!(c.statuses(), set(&[Status::Death]));
        }
        assert!(!(c.status.has(Status::Haste) && c.status.has(Status::Slow)), "{id:?}");
        assert!(!(c.status.has(Status::Sadness) && c.status.has(Status::Fury)), "{id:?}");
        for timed in [Status::Sleep, Status::Stop, Status::Paralysis, Status::Barrier, Status::MBarrier] {
            let left = c.status.points_left(timed);
            assert_eq!(left.is_some(), c.status.has(timed), "{id:?} {timed}");
            assert!(left.is_none_or(|points| points > 0), "a timer that has run out is a status that is over: {id:?} {timed}");
        }
        assert_eq!(c.status.regen_left() > 0, c.status.has(Status::Regen), "{id:?}");
        assert_eq!(state.ready_actors().contains(&id), i < state.party_ids().len() && c.ready && c.can_act(), "{id:?}");
        if c.status.pace() == status::Pace::Frozen {
            assert_eq!(state.gauge_increase(id), Some(0));
        }
    }
}

type Played = (BattleState, Vec<BattleEvent>);

/// A battle of random commands from the members, ticked in slices of `slice_ms`.
fn play(seed: u64, slice_ms: u32) -> Played {
    let mut state = status_battle(seed);
    let mut log = Vec::new();
    let mut pick = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
    for _ in 0..(600_000 / slice_ms) {
        if state.outcome().is_some() {
            break;
        }
        log.extend(state.step(slice_ms));
        assert_sane(&state);
        for id in state.ready_actors() {
            if state.outcome().is_some() {
                break;
            }
            let list = candidates(&state, id);
            let command = list[(lcg(&mut pick) % list.len() as u64) as usize];
            let before = state.clone();
            let checked = state.check_command(id, &command);
            let result = state.submit_command(id, command).map(|events| log.extend(events));
            assert_eq!(result, checked, "{command:?}: the check and the command agree");
            if checked.is_err() {
                assert_eq!(state, before, "a refusal changes nothing: {command:?}");
            }
            assert_sane(&state);
        }
    }
    (state, log)
}

#[test]
fn random_commands_on_a_battle_full_of_statuses_agree_with_their_checks_and_replay_exactly() {
    let mut seen: BTreeSet<Status> = BTreeSet::new();
    let (mut ticks, mut ends) = (0, 0);
    for seed in 0..12u64 {
        let (state, log) = play(seed, 100);
        let (again, log_again) = play(seed, 100);
        assert_eq!((&state, &log), (&again, &log_again), "seed {seed}: the same battle again");
        for event in &log {
            match event {
                BattleEvent::StatusInflicted { status, .. } => {
                    seen.insert(*status);
                }
                BattleEvent::StatusTick { .. } => ticks += 1,
                BattleEvent::StatusEnded { .. } => ends += 1,
                _ => {}
            }
        }
        // Whatever ended the battle is the last thing in the log, once.
        if state.outcome().is_some() {
            let last = log.last();
            assert!(matches!(last, Some(BattleEvent::Victory | BattleEvent::Defeat) | None), "seed {seed}: {last:?}");
            assert_eq!(log.iter().filter(|e| matches!(e, BattleEvent::Victory | BattleEvent::Defeat)).count(), 1);
        }
    }
    // The point of the battle is that it plays them: most of the statuses an enemy inflicts turn up, and the clocks
    // ran and ended some.
    for status in [Status::Poison, Status::Sleep, Status::Silence, Status::Slow, Status::Stop, Status::Darkness, Status::Haste, Status::Regen, Status::Sadness] {
        assert!(seen.contains(&status), "{status} never happened in 12 battles: {seen:?}");
    }
    assert!(ticks > 20 && ends > 20, "{ticks} turns, {ends} ends");
}

#[test]
fn a_battle_of_statuses_is_the_same_battle_however_time_is_sliced() {
    // No commands at all: the enemies strike, the statuses run. A tick is 1000/60 ms, so slices of 50, 100 and 1000
    // ms put the ticks in different calls and must change nothing.
    let run = |slice_ms: u32| {
        let mut state = status_battle(5);
        let mut log = Vec::new();
        for _ in 0..(40_000 / slice_ms) {
            log.extend(state.step(slice_ms));
        }
        (state, log)
    };
    let (state, log) = run(1000);
    assert!(log.iter().any(|e| matches!(e, BattleEvent::StatusInflicted { .. })));
    assert!(log.iter().any(|e| matches!(e, BattleEvent::StatusTick { .. })));
    for slice in [50, 100, 250] {
        let (other, other_log) = run(slice);
        assert_eq!((&other, &other_log), (&state, &log), "slices of {slice} ms");
    }
}

#[test]
fn a_recovery_with_a_status_of_its_own_heals_and_then_tries_it() {
    // Angel Whisper's kind: a fraction of the maximum HP and a cure of Poison, on one ally.
    let whisper = ActionSpec { hits_dead: false, ..ActionSpec::revive_spell(16, 0).with_aim(Aim::One) }.with_status(StatusEffect::cure(Status::Poison, 63));
    let mut hurt = with_status(hero(), &[Status::Poison]);
    hurt.hp = 100;
    let seed = 5;
    let mut state = battle(vec![hero(), hurt], vec![brute(5000)], seed);
    let mut events = Vec::new();
    state.resolve_action(0, &[1], &whisper, &mut events);
    assert!(matches!(events[..], [BattleEvent::Heal { target: B, .. }, BattleEvent::StatusEnded { target: B, status: Status::Poison }]), "{events:?}");
    assert!(!state.has_status(B, Status::Poison));
    // A fraction of the maximum has no variance: the one draw is the status roll.
    let mut replay = Rng::new(seed);
    replay.below(63);
    assert_eq!(state.rng, replay);
}
