//! Tests of whole-side targeting in the battle engine (protocol v10): `Target`, the Aim of an action, the
//! All materia rule, the split of damage and healing, and the order of what a whole side produces. The
//! rules are in the module docs of [`crate::rules`] ("Whole-side targeting") and [`crate::action`] ("Hitting
//! a whole side"). A child module of `rules`, so that it can read the generator of a battle and replay the
//! draws a command is documented to make.

use super::*;
use crate::action::{ItemData, ItemTable};
use crate::kernel::Kernel;
use crate::testutil::{attack_record, synthetic_kernel_bytes};

const A: ActorId = ActorId(0);

fn zero_start() -> BattleConfig {
    BattleConfig { start: StartMode::Zero, ..BattleConfig::default() }
}

#[allow(clippy::too_many_arguments)] // a table row of an attack record, spelled out
fn spell(index: u16, calc: u8, power: u8, accuracy: u8, targets: u8, element: u16, flags: u16, mp: u16) -> Spell {
    Spell::from_attack(index, &attack_record(calc, power, accuracy, targets, element, flags, mp)).expect("a castable spell")
}

/// Fire, Cure and Life as the real records have them (targeting `0x0F` and `0x0D`: one or a whole side).
fn fire() -> Spell {
    spell(27, 0x22, 8, 100, 0x0F, element::FIRE, 0xFDFF, 4)
}

fn cure() -> Spell {
    spell(0, 0x25, 5, 255, 0x0D, element::RESTORATIVE, 0xFDFF, 5)
}

fn life() -> Spell {
    spell(7, 0x24, 8, 255, 0x0D, element::RESTORATIVE, 0xFCFD, 34)
}

/// Ultima: a whole side only (`0x07`), 8 MP here so that the caster below can pay for it.
fn ultima() -> Spell {
    spell(52, 0x22, 105, 100, 0x07, 0, 0xFFFD, 8)
}

/// Level 10, 500 HP, 100 MP, Magic 20, Strength 30, the spells above and `all` uses of the All materia.
fn caster(all: u8) -> Combatant {
    let mut c = Combatant::new("Caster", 10, 500);
    c.mp = 100;
    c.max_mp = 100;
    c.magic = 20;
    c.attack = 30;
    c.defense = 10;
    c.dex = 40;
    c.spells = vec![cure(), fire(), life(), ultima()];
    c.all_left = all;
    c
}

/// An ally with `hp` of 500 hit points.
fn ally(hp: u32) -> Combatant {
    let mut c = Combatant::new("Ally", 10, 500);
    c.hp = hp;
    c.mp = 20;
    c.max_mp = 100;
    c.dex = 40;
    c
}

/// An enemy that stands there: `hp` hit points, no defense, magic defense 8, never acts.
fn dummy(hp: u32) -> Combatant {
    let mut c = Combatant::new("Dummy", 5, hp);
    c.magic_defense = 8;
    c
}

fn items() -> Arc<ItemTable> {
    Arc::new(ItemTable::from_kernel(&Kernel::parse(&synthetic_kernel_bytes()).unwrap()))
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

fn magic(spell: u16, target: Target) -> Command {
    Command::Magic { spell, target }
}

fn rolls(variance: u32) -> Rolls {
    Rolls { critical: false, variance }
}

/// The numbers by hand: Fire at variance 255 is `(10 + 20) * 6 * (512 - 8) * 8 >> 13` = 88 to one target;
/// to several it is 2/3 of that before the variance, 88 * 2 / 3 = 58. Cure's base is `22 * 5 + 6 * (10 + 20)`
/// = 290 and 193 split. A physical blow of Power 16 is 39 and 26 split.
#[test]
fn the_split_is_two_thirds_for_the_formulas_that_split_and_nothing_for_one_target() {
    let (me, dummy) = (caster(1), dummy(1000));
    let fire = fire().spec;
    assert_eq!(action_damage(&fire, &me, &dummy, Elemental::Normal, rolls(255)), 88);
    assert_eq!(action_damage_among(&fire, &me, &dummy, Elemental::Normal, rolls(255), 1), 88, "one target is never split");
    assert_eq!(action_damage_among(&fire, &me, &dummy, Elemental::Normal, rolls(255), 0), 88);
    assert_eq!(action_damage_among(&fire, &me, &dummy, Elemental::Normal, rolls(255), 2), 58);
    assert_eq!(action_damage_among(&fire, &me, &dummy, Elemental::Normal, rolls(255), 9), 58, "2/3 whatever the number");
    // The variance comes after the split: 58 * 3841 >> 12 = 54; the element after it: doubled before the variance.
    assert_eq!(action_damage_among(&fire, &me, &dummy, Elemental::Normal, rolls(0), 3), 54);
    assert_eq!(action_damage_among(&fire, &me, &dummy, Elemental::Double, rolls(255), 3), 116, "(88 * 2 / 3) * 2");
    assert_eq!(action_damage_among(&fire, &me, &dummy, Elemental::Half, rolls(255), 3), 29, "(88 * 2 / 3) / 2");

    let wind = cure().spec;
    assert_eq!(restore_amount(&wind, &me, &ally(1), 255), 290);
    assert_eq!(restore_amount_among(&wind, &me, &ally(1), 255, 3), 193);
    assert_eq!(restore_amount_among(&wind, &me, &ally(1), 0, 3), 180, "193 * 3841 >> 12");
    assert_eq!(restore_amount_among(&wind, &me, &ally(1), 255, 1), 290);

    // The physical formula splits too (an enemy that hits the whole party), Defend halves before the split.
    let blow = ActionSpec::basic_attack(16, 100, false).with_aim(Aim::Side);
    assert_eq!(action_damage(&blow, &me, &dummy, Elemental::Normal, rolls(255)), 39);
    assert_eq!(action_damage_among(&blow, &me, &dummy, Elemental::Normal, rolls(255), 4), 26);
    let mut guard = dummy.clone();
    guard.defending = true;
    assert_eq!(action_damage_among(&blow, &me, &guard, Elemental::Normal, rolls(255), 4), 12, "(39 / 2) * 2 / 3 = 19 * 2 / 3");

    // No split: a fraction of HP (Life, Demi), a flat amount (Potion) and the item formula; and a spell that
    // is a forced whole side (No Split).
    let life = life().spec;
    assert_eq!(restore_amount_among(&life, &me, &ally(1), 0, 3), restore_amount(&life, &me, &ally(1), 0));
    let potion = ActionSpec::flat_restore_item(5, false);
    assert_eq!(restore_amount_among(&potion, &me, &ally(1), 0, 3), 100);
    let shrapnel = ActionSpec::damage_item(10, 0).with_aim(Aim::Side);
    assert_eq!(
        action_damage_among(&shrapnel, &me, &dummy, Elemental::Normal, rolls(255), 3),
        action_damage(&shrapnel, &me, &dummy, Elemental::Normal, rolls(255))
    );
    let ultima = ultima().spec;
    assert_eq!(
        action_damage_among(&ultima, &me, &dummy, Elemental::Normal, rolls(255), 3),
        action_damage(&ultima, &me, &dummy, Elemental::Normal, rolls(255)),
        "Ultima is All Tar (NS): full strength on every enemy"
    );
}

#[test]
fn fire_on_every_enemy_hits_each_for_two_thirds_with_its_own_rolls_in_actor_order() {
    let seed = 31;
    let mut state = battle(vec![caster(1)], vec![dummy(1000), dummy(1000), dummy(1000)], seed);
    wait_ready(&mut state, A);
    let events = state.submit_command(A, magic(27, Target::AllEnemies)).unwrap();
    // The draws, replayed: for each target in actor order the roll to hit (a magical action has no luck roll),
    // then the variance. Nothing else is drawn: the split and the All use are arithmetic.
    let mut replay = Rng::new(seed);
    let mut expected = vec![
        BattleEvent::Action { actor: A, action: ActionRef::Magic(27) },
        BattleEvent::MpSpent { actor: A, amount: 4 },
    ];
    for target in [1u8, 2, 3] {
        assert!(percent_roll(replay.next_u16()) <= 100, "Fire's 100% cannot miss");
        let variance = replay.range_inclusive(0, 255);
        let amount = action_damage_among(&fire().spec, &caster(1), &dummy(1000), Elemental::Normal, rolls(variance), 3);
        assert!((54..=58).contains(&amount), "two thirds of 82..=88: {amount}");
        expected.push(BattleEvent::Damage { actor: A, target: ActorId(target), amount, critical: false });
    }
    assert_eq!(events, expected);
    assert_eq!(state.rng, replay);
    // The MP is paid once, the All use is spent, the enemies lost exactly what the events say.
    assert_eq!(state.actor(A).unwrap().mp, 96);
    assert_eq!((state.all_left(A), state.actor(A).unwrap().all_left), (0, 0));
    for (i, event) in expected[2..].iter().enumerate() {
        let BattleEvent::Damage { amount, .. } = event else { unreachable!() };
        assert_eq!(state.actors()[i + 1].hp, 1000 - amount);
    }
}

#[test]
fn a_whole_side_with_one_enemy_left_is_not_split_but_still_spends_the_all_use() {
    let seed = 7;
    let mut state = battle(vec![caster(1)], vec![dummy(1000)], seed);
    wait_ready(&mut state, A);
    let events = state.submit_command(A, magic(27, Target::AllEnemies)).unwrap();
    let mut replay = Rng::new(seed);
    let _ = replay.next_u16();
    let variance = replay.range_inclusive(0, 255);
    let alone = action_damage(&fire().spec, &caster(1), &dummy(1000), Elemental::Normal, rolls(variance));
    assert_eq!(events[2], BattleEvent::Damage { actor: A, target: ActorId(1), amount: alone, critical: false });
    assert!(alone >= 82, "the whole of Fire: {alone}");
    assert_eq!(state.all_left(A), 0, "the choice of All spent a use, whatever was left to hit");
}

#[test]
fn cure_on_the_whole_party_heals_each_living_ally_at_two_thirds_and_never_the_fallen() {
    let seed = 5;
    let mut fallen = ally(0);
    fallen.name = "Fallen".into();
    let mut state = battle(vec![caster(1), ally(100), fallen, ally(480)], vec![dummy(1000)], seed);
    // The caster is hurt too.
    state.actors[0].hp = 300;
    wait_ready(&mut state, A);
    let events = state.submit_command(A, magic(0, Target::AllAllies)).unwrap();
    // Cure draws one variance per target (it never misses), for the living ones in actor order: 0, 1 and 3.
    let mut replay = Rng::new(seed);
    let mut expected = vec![
        BattleEvent::Action { actor: A, action: ActionRef::Magic(0) },
        BattleEvent::MpSpent { actor: A, amount: 5 },
    ];
    for (target, missing) in [(0u8, 200u32), (1, 400), (3, 20)] {
        let variance = replay.range_inclusive(0, 255);
        let gives = restore_amount_among(&cure().spec, &caster(1), &ally(1), variance, 3);
        assert!((180..=193).contains(&gives), "two thirds of 271..=290: {gives}");
        expected.push(BattleEvent::Heal { actor: A, target: ActorId(target), amount: gives.min(missing) });
    }
    assert_eq!(events, expected);
    assert_eq!(state.rng, replay);
    assert_eq!(state.actors()[2].hp, 0, "the fallen stay fallen");
    assert_eq!(state.all_left(A), 0);
}

#[test]
fn life_on_the_whole_party_revives_every_fallen_ally_and_needs_one_to_be_fallen() {
    let mut first = ally(0);
    first.max_hp = 400;
    let mut state = battle(vec![caster(2), first, ally(500), ally(0)], vec![dummy(1000)], 3);
    wait_ready(&mut state, A);
    let events = state.submit_command(A, magic(7, Target::AllAllies)).unwrap();
    // A fraction of max HP (8/32) does not split: 100 for the 400 and 125 for the 500, the gauges start empty.
    assert_eq!(
        events,
        [
            BattleEvent::Action { actor: A, action: ActionRef::Magic(7) },
            BattleEvent::MpSpent { actor: A, amount: 34 },
            BattleEvent::Revive { actor: A, target: ActorId(1), hp: 100 },
            BattleEvent::Revive { actor: A, target: ActorId(3), hp: 125 },
        ]
    );
    // Everyone stands now: there is nobody to revive.
    wait_ready(&mut state, A);
    assert_eq!(state.check_command(A, &magic(7, Target::AllAllies)), Err(CommandError::NoTargets(Target::AllAllies)));
    assert_eq!(state.submit_command(A, magic(7, Target::AllAllies)), Err(CommandError::NoTargets(Target::AllAllies)));
    // One use was spent: the second would be refused only for want of a fallen ally here.
    assert_eq!(state.all_left(A), 1);
}

#[test]
fn a_forced_whole_side_spell_needs_no_all_use_is_not_split_and_cannot_aim_at_one() {
    let seed = 11;
    // No All materia at all, and Ultima still hits every enemy.
    let mut state = battle(vec![caster(0)], vec![dummy(4000), dummy(4000)], seed);
    wait_ready(&mut state, A);
    assert_eq!(state.check_command(A, &magic(52, Target::One(ActorId(1)))), Err(CommandError::InvalidReach(Target::One(ActorId(1)))));
    assert_eq!(state.check_command(A, &magic(52, Target::AllAllies)), Err(CommandError::InvalidReach(Target::AllAllies)));
    let events = state.submit_command(A, magic(52, Target::AllEnemies)).unwrap();
    let mut replay = Rng::new(seed);
    let mut expected = vec![
        BattleEvent::Action { actor: A, action: ActionRef::Magic(52) },
        BattleEvent::MpSpent { actor: A, amount: 8 },
    ];
    for target in [1u8, 2] {
        let _ = replay.next_u16();
        let variance = replay.range_inclusive(0, 255);
        let amount = action_damage(&ultima().spec, &caster(0), &dummy(4000), Elemental::Normal, rolls(variance));
        expected.push(BattleEvent::Damage { actor: A, target: ActorId(target), amount, critical: false });
    }
    assert_eq!(events, expected);
    assert_eq!(state.all_left(A), 0);
    assert_eq!(state.rng, replay);
}

#[test]
fn the_all_materia_lets_a_toggle_spell_hit_a_whole_side_so_many_times_per_battle() {
    let mut state = battle(vec![caster(2), ally(500)], vec![dummy(9000), dummy(9000)], 1);
    assert_eq!((state.all_left(A), state.all_left(ActorId(1))), (2, 0));
    assert_eq!((state.all_left(ActorId(2)), state.all_left(ActorId(77))), (0, 0), "an enemy and an unknown actor have none");
    // A caster with no All cannot aim a toggle spell at a whole side, in any way a command can ask.
    let mut plain = battle(vec![caster(0)], vec![dummy(9000), dummy(9000)], 1);
    wait_ready(&mut plain, A);
    let before = plain.clone();
    for (spell, target) in [(27, Target::AllEnemies), (0, Target::AllAllies), (7, Target::AllAllies)] {
        let expect = match spell {
            7 => CommandError::NoTargets(Target::AllAllies), // the reach is checked first: nobody is fallen
            _ => CommandError::NoAllLeft(A),
        };
        assert_eq!(plain.check_command(A, &magic(spell, target)), Err(expect), "spell {spell}");
        assert_eq!(plain.submit_command(A, magic(spell, target)), Err(expect));
        assert_eq!(plain, before, "a refusal changes nothing");
    }
    // One target is always allowed.
    assert!(plain.check_command(A, &magic(27, Target::One(ActorId(1)))).is_ok());

    // With two uses: the first and the second work, the third is refused; one-target casts in between cost none.
    wait_ready(&mut state, A);
    assert!(state.submit_command(A, magic(27, Target::AllEnemies)).is_ok());
    assert_eq!(state.all_left(A), 1);
    wait_ready(&mut state, A);
    assert!(state.submit_command(A, magic(27, Target::One(ActorId(2)))).is_ok());
    assert_eq!(state.all_left(A), 1, "aiming at one target is free");
    wait_ready(&mut state, A);
    assert!(state.submit_command(A, magic(0, Target::AllAllies)).is_ok());
    assert_eq!(state.all_left(A), 0);
    wait_ready(&mut state, A);
    assert_eq!(state.check_command(A, &magic(27, Target::AllEnemies)), Err(CommandError::NoAllLeft(A)));
    assert!(state.check_command(A, &magic(27, Target::One(ActorId(2)))).is_ok());
}

#[test]
fn an_item_or_a_limit_that_can_hit_a_whole_side_needs_no_all_use() {
    // A hand-made item of the toggle kind (no real record has one) and Megalixir, with a caster that has no All.
    let toggle = ActionSpec::damage_item(10, 0).with_aim(Aim::OneOrSide);
    let table = Arc::new(ItemTable::new([(50, ItemData::new(toggle))]));
    let mut state = BattleState::with_config(1, vec![caster(0)], vec![dummy(900), dummy(900)], zero_start()).with_items(table);
    wait_ready(&mut state, A);
    let one = Command::Item { item: 50, target: Target::One(ActorId(1)) };
    let side = Command::Item { item: 50, target: Target::AllEnemies };
    assert_eq!((state.check_command(A, &one), state.check_command(A, &side)), (Ok(()), Ok(())));
    let events = state.submit_command(A, side).unwrap();
    let hits: Vec<&BattleEvent> = events.iter().filter(|e| matches!(e, BattleEvent::Damage { .. })).collect();
    assert_eq!(hits.len(), 2, "every enemy: {events:?}");
    assert_eq!(state.all_left(A), 0);
}

#[test]
fn megalixir_restores_hp_and_mp_of_every_living_ally_and_a_side_item_hits_every_enemy_unsplit() {
    let mut low = ally(100);
    low.mp = 0;
    let mut fallen = ally(0);
    fallen.mp = 0;
    let mut state = battle(vec![caster(0), low, fallen, ally(500)], vec![dummy(1000), dummy(1000)], 9);
    state.actors[0].hp = 250;
    wait_ready(&mut state, A);
    let events = state.submit_command(A, Command::Item { item: 6, target: Target::AllAllies }).unwrap();
    // Everything, for each living ally in actor order: HP then MP; the fallen are not touched (that is the
    // job of a revival). Nobody is split.
    assert_eq!(
        events,
        [
            BattleEvent::Action { actor: A, action: ActionRef::Item(6) },
            BattleEvent::Heal { actor: A, target: A, amount: 250 },
            BattleEvent::MpRestored { actor: A, target: A, amount: 0 },
            BattleEvent::Heal { actor: A, target: ActorId(1), amount: 400 },
            BattleEvent::MpRestored { actor: A, target: ActorId(1), amount: 100 },
            BattleEvent::Heal { actor: A, target: ActorId(3), amount: 0 },
            BattleEvent::MpRestored { actor: A, target: ActorId(3), amount: 80 },
        ]
    );
    assert_eq!(state.actors()[2].hp, 0);

    // A damaging item that hits every enemy (Shrapnel's kind): per enemy what one would take, in actor order.
    let shrapnel = ActionSpec::damage_item(26, element::SHOOT).with_aim(Aim::Side);
    let table = Arc::new(ItemTable::new([(21, ItemData::new(shrapnel))]));
    let seed = 4;
    let mut state = BattleState::with_config(seed, vec![caster(0)], vec![dummy(900), dummy(900)], zero_start()).with_items(table);
    wait_ready(&mut state, A);
    let events = state.submit_command(A, Command::Item { item: 21, target: Target::AllEnemies }).unwrap();
    let mut replay = Rng::new(seed);
    let mut expected = vec![BattleEvent::Action { actor: A, action: ActionRef::Item(21) }];
    for target in [1u8, 2] {
        // The item formula has a variance and no roll to hit.
        let variance = replay.range_inclusive(0, 255);
        let amount = action_damage(&shrapnel, &caster(0), &dummy(900), Elemental::Normal, rolls(variance));
        expected.push(BattleEvent::Damage { actor: A, target: ActorId(target), amount, critical: false });
    }
    assert_eq!(events, expected);
    assert_eq!(state.rng, replay);
    wait_ready(&mut state, A);
    assert_eq!(state.check_command(A, &Command::Item { item: 21, target: Target::One(ActorId(1)) }), Err(CommandError::InvalidReach(Target::One(ActorId(1)))));
}

#[test]
fn a_whole_side_that_ends_the_battle_ends_it_on_the_victory_after_the_last_blow() {
    // Both enemies fall to one blow each: Damage, Death, Damage, Death and then the Victory, nothing after.
    let mut state = battle(vec![caster(1)], vec![dummy(1), dummy(1)], 2);
    wait_ready(&mut state, A);
    let events = state.submit_command(A, magic(27, Target::AllEnemies)).unwrap();
    let kinds: Vec<&str> = events
        .iter()
        .map(|e| match e {
            BattleEvent::Action { .. } => "action",
            BattleEvent::MpSpent { .. } => "mp",
            BattleEvent::Damage { .. } => "damage",
            BattleEvent::Death { .. } => "death",
            BattleEvent::Victory => "victory",
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(kinds, ["action", "mp", "damage", "death", "damage", "death", "victory"]);
    assert_eq!(state.outcome(), Some(Outcome::Victory));
    // One survivor: no victory, and the dead enemy is not hit twice by a second cast.
    let mut state = battle(vec![caster(2)], vec![dummy(1), dummy(5000)], 2);
    wait_ready(&mut state, A);
    let events = state.submit_command(A, magic(27, Target::AllEnemies)).unwrap();
    assert!(!events.contains(&BattleEvent::Victory) && state.outcome().is_none());
    wait_ready(&mut state, A);
    let again = state.submit_command(A, magic(27, Target::AllEnemies)).unwrap();
    let hit: Vec<ActorId> = again.iter().filter_map(|e| if let BattleEvent::Damage { target, .. } = e { Some(*target) } else { None }).collect();
    assert_eq!(hit, [ActorId(2)], "only the living are targets");
}

#[test]
fn a_whole_side_is_checked_for_the_side_it_aims_at_and_the_way_the_action_allows() {
    let mut state = battle(vec![caster(5), ally(500)], vec![dummy(1000)], 6);
    wait_ready(&mut state, A);
    let one = |id| Target::One(ActorId(id));
    let table: [(Command, Result<(), CommandError>); 14] = [
        // Restoring spells aim at the allies, damaging ones at the enemies, whole sides included.
        (magic(0, Target::AllAllies), Ok(())),
        (magic(0, Target::AllEnemies), Err(CommandError::InvalidReach(Target::AllEnemies))),
        (magic(27, Target::AllEnemies), Ok(())),
        (magic(27, Target::AllAllies), Err(CommandError::InvalidReach(Target::AllAllies))),
        // One target keeps its checks, in their old order.
        (magic(0, one(1)), Ok(())),
        (magic(0, one(2)), Err(CommandError::InvalidTarget(ActorId(2)))),
        (magic(27, one(9)), Err(CommandError::UnknownTarget(ActorId(9)))),
        (magic(27, one(0)), Err(CommandError::InvalidTarget(A))),
        // Ultima is a whole side only; Life a revival: nobody is fallen.
        (magic(52, one(2)), Err(CommandError::InvalidReach(one(2)))),
        (magic(52, Target::AllEnemies), Ok(())),
        (magic(7, Target::AllAllies), Err(CommandError::NoTargets(Target::AllAllies))),
        (magic(7, one(1)), Err(CommandError::TargetNotDead(ActorId(1)))),
        // The order of the checks: a spell the actor does not know, then its MP, then the way it is aimed.
        (magic(99, Target::AllEnemies), Err(CommandError::UnknownSpell(99))),
        (Command::Item { item: 6, target: one(1) }, Err(CommandError::InvalidReach(one(1)))),
    ];
    let before = state.clone();
    for (command, expected) in &table {
        assert_eq!(state.check_command(A, command), *expected, "{command:?}");
        assert_eq!(state.submit_command(A, *command).map(|_| ()), *expected, "{command:?}");
        if expected.is_err() {
            assert_eq!(state, before, "a refusal changes nothing: {command:?}");
        } else {
            // Give the turn back for the next row.
            state = before.clone();
        }
    }
    // MP comes before the way of aiming.
    let mut poor = caster(1);
    poor.mp = 3;
    let mut state = battle(vec![poor], vec![dummy(1000)], 6);
    wait_ready(&mut state, A);
    assert_eq!(state.check_command(A, &magic(27, Target::AllAllies)), Err(CommandError::NotEnoughMp(A)));
}

/// An enemy's action on several targets, the way the AI of an enemy will ask for it: the party members it
/// hits are resolved in order, the physical formula splits, and what a blow does to a limit gauge follows each.
#[test]
fn an_enemy_action_on_the_whole_party_is_resolved_target_by_target() {
    let strong = |name: &str| {
        let mut c = Combatant::new(name, 10, 800);
        c.defense = 0;
        c.dex = 40;
        c.limit_break = Some(LimitBreak::new(128, ActionSpec::limit_strike(48), 140));
        c
    };
    let mut ogre = Combatant::new("Ogre", 20, 5000);
    ogre.attack = 80;
    let mut state = BattleState::with_config(5, vec![strong("A"), strong("B"), strong("C")], vec![ogre], zero_start());
    let blow = ActionSpec::basic_attack(16, 255, false).with_aim(Aim::Side);
    let mut events = Vec::new();
    state.resolve_action(3, &[0, 1, 2], &blow, &mut events);
    let blows: Vec<(ActorId, u32)> = events
        .iter()
        .filter_map(|e| match e {
            BattleEvent::Damage { target, amount, .. } => Some((*target, *amount)),
            _ => None,
        })
        .collect();
    assert_eq!(blows.iter().map(|(t, _)| t.0).collect::<Vec<_>>(), [0, 1, 2], "in actor order");
    // Each hit is a third less than the same blow on one party member would be (at the same variance).
    let single = {
        let mut one = events.clone();
        one.clear();
        let mut alone = BattleState::with_config(5, vec![strong("A")], vec![state.actors()[3].clone()], zero_start());
        alone.resolve_action(1, &[0], &blow, &mut one);
        one.iter().find_map(|e| if let BattleEvent::Damage { amount, .. } = e { Some(*amount) } else { None }).unwrap()
    };
    assert!(blows.iter().all(|(_, amount)| *amount < single), "{blows:?} against {single}");
    // The limit gauge of each member follows its own blow: Damage, LimitGauge, Damage, LimitGauge, ...
    let shape: Vec<&str> = events
        .iter()
        .map(|e| match e {
            BattleEvent::Damage { .. } => "damage",
            BattleEvent::LimitGauge { .. } => "gauge",
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(shape, ["damage", "gauge", "damage", "gauge", "damage", "gauge"]);
    assert!(state.actors()[..3].iter().all(|c| c.limit > 0));

    // The same words from the enemy's side: its enemies are the party, its allies the enemy side (itself
    // included), and a party member's words are what they were.
    let strike = ActionSpec::basic_attack(16, 100, false);
    let heal = ActionSpec::cure_spell(5, 0);
    let ogre = 3;
    assert_eq!(state.aim_at(ogre, &blow, Target::AllEnemies), Ok(vec![0, 1, 2]));
    assert_eq!(state.aim_at(ogre, &blow, Target::One(ActorId(1))), Err(CommandError::InvalidReach(Target::One(ActorId(1)))), "a blow that only hits a whole side");
    assert_eq!(state.aim_at(ogre, &strike, Target::One(ActorId(1))), Ok(vec![1]));
    assert_eq!(state.aim_at(ogre, &strike, Target::One(ActorId(3))), Err(CommandError::InvalidTarget(ActorId(3))), "not its own side");
    assert_eq!(state.aim_at(ogre, &heal, Target::AllAllies), Ok(vec![3]), "its own side is the ogre alone");
    assert_eq!(state.aim_at(ogre, &heal, Target::AllEnemies), Err(CommandError::InvalidReach(Target::AllEnemies)));
    assert_eq!(state.aim_at(0, &heal, Target::AllAllies), Ok(vec![0, 1, 2]));
    assert_eq!(state.aim_at(0, &strike, Target::One(ActorId(3))), Ok(vec![3]));
}

/// A deterministic policy that favours the whole-side commands the party has, for the replay test below.
fn side_policy(state: &BattleState, actor: ActorId) -> Command {
    let enemy = state.enemy_ids().into_iter().find(|e| state.actor(*e).is_some_and(Combatant::is_alive));
    let candidates = [
        Some(magic(27, Target::AllEnemies)),
        Some(magic(0, Target::AllAllies)),
        Some(Command::Item { item: 6, target: Target::AllAllies }),
        Some(magic(7, Target::AllAllies)),
        Some(magic(52, Target::AllEnemies)),
        enemy.map(|target| Command::Attack { target }),
    ];
    let pick = (state.tick() / 17 + u64::from(actor.0)) as usize % candidates.len();
    candidates[pick]
        .filter(|c| state.check_command(actor, c).is_ok())
        .or(enemy.map(|target| Command::Attack { target }))
        .unwrap_or(Command::Defend)
}

fn side_battle(seed: u64) -> BattleState {
    let goblin = |name: &str| {
        let mut c = Combatant::new(name, 8, 900);
        c.attack = 50;
        c.defense = 12;
        c.magic_defense = 10;
        c.dex = 45;
        c.accuracy = 120;
        c.reward = Reward { exp: 20, gil: 30, ap: 2 };
        c
    };
    let mut first = caster(3);
    first.hp = 500;
    let mut second = caster(2);
    second.name = "Second".into();
    BattleState::with_config(
        seed,
        vec![first, second, ally(0)],
        vec![goblin("Goblin A"), goblin("Goblin B"), goblin("Goblin C")],
        zero_start(),
    )
    .with_items(items())
}

#[test]
fn battles_of_whole_side_commands_replay_exactly_and_stay_sane() {
    let play = |seed: u64| {
        let mut state = side_battle(seed);
        let mut log = Vec::new();
        let mut sides = 0;
        for _ in 0..20_000 {
            if state.outcome().is_some() {
                break;
            }
            log.extend(state.step(100));
            for id in state.ready_actors() {
                if state.outcome().is_some() {
                    break;
                }
                let command = side_policy(&state, id);
                if matches!(command, Command::Magic { target, .. } | Command::Item { target, .. } if target.is_side()) {
                    sides += 1;
                }
                let checked = state.check_command(id, &command);
                let events = state.submit_command(id, command).unwrap_or_else(|e| panic!("{command:?}: {e} (checked {checked:?})"));
                assert_eq!(checked, Ok(()));
                log.extend(events);
                for c in state.actors() {
                    assert!(c.hp <= c.max_hp && c.mp <= c.max_mp && c.all_left <= 3);
                }
            }
        }
        (state, log, sides)
    };
    let mut total_sides = 0;
    for seed in 0..16u64 {
        let (a, log_a, sides) = play(seed);
        let (b, log_b, _) = play(seed);
        assert_eq!((&a, &log_a), (&b, &log_b), "seed {seed}");
        assert!(a.outcome().is_some(), "seed {seed} never ended");
        let ends = log_a.iter().filter(|e| matches!(e, BattleEvent::Victory | BattleEvent::Defeat)).count();
        assert_eq!(ends, 1, "seed {seed}");
        assert!(matches!(log_a.last(), Some(BattleEvent::Victory | BattleEvent::Defeat)), "nothing happens after the end");
        total_sides += sides;
    }
    assert!(total_sides > 16, "only {total_sides} whole-side commands were given");
}

#[test]
fn a_character_from_the_kernel_starts_with_the_uses_of_its_all_materia() {
    let kernel = Kernel::parse(&synthetic_kernel_bytes()).unwrap();
    let cloud = Combatant::from_character(&kernel, CharacterId::Cloud, 6, None, None).unwrap();
    assert_eq!(cloud.all_left, 1, "an All materia at level 1: once per battle");
    assert_eq!(kernel.materia(STARTER_ALL_MATERIA).map(|m| m.all_uses(1)), Some(1));
    // Enemies and hand-made combatants have none; a kernel without the record gives none.
    assert_eq!(Combatant::new("Hand-made", 1, 1).all_left, 0);
    let mut sections = crate::testutil::synthetic_kernel_sections();
    sections[8].1 = vec![0xFF; crate::kernel::MATERIA_COUNT * 20];
    let bare = Kernel::parse(&crate::testutil::pack_kernel(&sections, &[])).unwrap();
    let bare_cloud = Combatant::from_character(&bare, CharacterId::Cloud, 6, None, None).unwrap();
    assert_eq!(bare_cloud.all_left, 0);
    // The battle builds with whatever the combatant carries, and never more than the All materia's own five.
    let mut greedy = Combatant::new("Greedy", 5, 100);
    greedy.all_left = 200;
    assert_eq!(BattleState::new(1, vec![greedy.clone()], vec![dummy(10)]).all_left(A), 200, "an input like the level; the data caps it");
    assert_eq!(crate::kernel::MAX_ALL_USES, 5);
}

#[test]
fn targets_have_the_words_the_wire_gives_them() {
    for (target, json) in [
        (Target::One(ActorId(3)), r#"{"One":3}"#),
        (Target::AllEnemies, r#""AllEnemies""#),
        (Target::AllAllies, r#""AllAllies""#),
    ] {
        assert_eq!(serde_json::to_string(&target).unwrap(), json);
        assert_eq!(serde_json::from_str::<Target>(json).unwrap(), target);
    }
    assert_eq!(Target::One(ActorId(2)).one(), Some(ActorId(2)));
    assert_eq!((Target::AllEnemies.one(), Target::AllAllies.one()), (None, None));
    assert!(!Target::One(A).is_side() && Target::AllEnemies.is_side() && Target::AllAllies.is_side());
    // A bare number is the old (protocol v9) shape of a target: it is not one now.
    assert!(serde_json::from_str::<Target>("3").is_err());
    assert!(serde_json::from_str::<Command>(r#"{"Magic":{"spell":0,"target":3}}"#).is_err());
    assert!(serde_json::from_str::<Command>(r#"{"Magic":{"spell":0,"target":{"One":3}}}"#).is_ok());
}
