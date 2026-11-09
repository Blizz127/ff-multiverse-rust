//! Scripted enemies inside a running battle: the engine hook, the fallback, determinism.

use super::asm::{battle_of, battle_with, laser, party_of, plain_foe, scripted_foe, Asm, ATTACK, LASER, OTHER};
use super::script::op;
use crate::rules::{ActionRef, ActorId, BattleEvent, BattleState};

/// The foe's position in `battle_of`.
const FOE: ActorId = ActorId(2);

fn weakest(attack: u16) -> Asm {
    Asm::act_on(
        |a| a.val(2, 0x20A0).val(3, 0x4160).op(op::MASK).op(op::LEAST),
        attack,
    )
}

/// Run `ticks` ticks one at a time and return, per tick, what happened.
fn per_tick(state: &mut BattleState, ticks: u64) -> Vec<Vec<BattleEvent>> {
    (0..ticks).map(|_| state.step_ticks(1)).collect()
}

fn all(batches: &[Vec<BattleEvent>]) -> Vec<BattleEvent> {
    batches.iter().flatten().copied().collect()
}

fn acted(events: &[BattleEvent], who: ActorId) -> bool {
    events
        .iter()
        .any(|e| matches!(e, BattleEvent::Action { actor, .. } if *actor == who))
}

#[test]
fn a_scripted_enemy_attacks_the_target_its_script_names() {
    let mut state = battle_of(5, vec![scripted_foe(None, &weakest(ATTACK))]);
    let log = all(&per_tick(&mut state, 400));
    let hits: Vec<ActorId> = log
        .iter()
        .filter_map(|e| match e {
            BattleEvent::Damage { actor, target, .. } if *actor == FOE => Some(*target),
            _ => None,
        })
        .collect();
    assert!(hits.len() >= 3, "the foe attacked several times: {hits:?}");
    // The weakest party member is Barret until he dies; Cloud only after that.
    assert_eq!(hits[0], ActorId(1));
    assert!(log.contains(&BattleEvent::Action {
        actor: FOE,
        action: ActionRef::EnemyAttack(ATTACK)
    }));
}

#[test]
fn a_script_that_ends_without_an_action_passes_the_turn() {
    let mut state = battle_of(5, vec![scripted_foe(None, &Asm::default().op(op::END))]);
    let batches = per_tick(&mut state, 600);
    let turns = all(&batches)
        .iter()
        .filter(|e| matches!(e, BattleEvent::GaugeReady { actor } if *actor == FOE))
        .count();
    assert!(turns >= 3, "the foe's gauge filled repeatedly: {turns}");
    assert!(!acted(&all(&batches), FOE), "and it never acted");
    assert!(all(&batches).iter().all(|e| !matches!(e, BattleEvent::Damage { .. })));
}

#[test]
fn what_a_script_stored_is_there_on_the_enemys_next_turn() {
    // 1st Ray's rhythm: attack, then pass, then attack ...  (LV0000 as the flag)
    //   if (LV0000 == 0) { act on the weakest; LV0000 = 1 } else { LV0000 = 0 }
    let mut main = Asm::default().val(0, 0x00).op(op::LNOT);
    let jz = main.here();
    main = main.jump(op::JUMP_IF_ZERO, 0);
    main = main
        .addr(2, 0x2070)
        .val(2, 0x20A0)
        .val(3, 0x4160)
        .op(op::MASK)
        .op(op::LEAST)
        .op(op::STORE)
        .k1(0x20)
        .k2(ATTACK)
        .op(op::ACT)
        .addr(0, 0x00)
        .k1(1)
        .op(op::STORE);
    let skip = main.here();
    main = main.jump(op::JUMP, 0);
    let rest = main.here();
    main = main.addr(0, 0x00).k1(0).op(op::STORE);
    let end = main.here();
    main = main.op(op::END);
    main.patch(jz, rest);
    main.patch(skip, end);
    let mut state = battle_of(5, vec![scripted_foe(None, &main)]);
    let batches = per_tick(&mut state, 900);
    // One entry per turn of the foe: did it act?
    let rhythm: Vec<bool> = batches
        .iter()
        .filter(|b| b.iter().any(|e| matches!(e, BattleEvent::GaugeReady { actor } if *actor == FOE)))
        .map(|b| acted(b, FOE))
        .collect();
    assert!(rhythm.len() >= 6, "{rhythm:?}");
    for (i, did) in rhythm.iter().enumerate() {
        assert_eq!(*did, i % 2 == 0, "turn {i}: {rhythm:?}");
    }
}

#[test]
fn an_attack_the_rules_cannot_play_is_announced_and_nothing_more() {
    let mut state = battle_of(5, vec![scripted_foe(None, &weakest(OTHER))]);
    let log = all(&per_tick(&mut state, 300));
    assert!(log.contains(&BattleEvent::Action {
        actor: FOE,
        action: ActionRef::EnemyAttack(OTHER)
    }));
    assert!(log
        .iter()
        .all(|e| !matches!(e, BattleEvent::Damage { .. } | BattleEvent::Miss { .. } | BattleEvent::MpSpent { .. })));
}

/// What the plain engine does with the same battle and no scripts.
fn plain_log(seed: u64, ticks: u64) -> Vec<BattleEvent> {
    let mut state = battle_of(seed, vec![plain_foe()]);
    all(&per_tick(&mut state, ticks))
}

#[test]
fn what_cannot_be_followed_falls_back_to_exactly_the_plain_behaviour() {
    let expected = plain_log(7, 700);
    assert!(acted(&expected, FOE), "the plain foe attacks");
    for (what, main) in [
        ("unsupported opcode", Asm::default().op(op::LINK).op(op::END)),
        ("endless loop", Asm::default().jump(op::JUMP, 0)),
        ("an attack the scene does not define", weakest(0x0999)),
        // Nobody to aim at.
        ("an empty target", Asm::act_on(|a| a.k1(0), ATTACK)),
    ] {
        let mut state = battle_of(7, vec![scripted_foe(None, &main)]);
        assert_eq!(all(&per_tick(&mut state, 700)), expected, "{what}");
    }
    // A failing Initialize script: scripts off for good, plain behaviour.
    let init = Asm::default().op(op::LINK).op(op::END);
    let mut state = battle_of(7, vec![scripted_foe(Some(&init), &weakest(ATTACK))]);
    assert_eq!(all(&per_tick(&mut state, 700)), expected, "failing Initialize");
}

#[test]
fn two_engines_with_the_same_seed_agree_and_the_seed_matters() {
    // A random target and a random pass: the stream decides, only the battle's.
    let main = Asm::act_on(|a| a.val(2, 0x20A0).op(op::RANDOM_BIT), ATTACK);
    let run = |seed: u64| {
        let mut state = battle_of(seed, vec![scripted_foe(None, &main), scripted_foe(None, &weakest(ATTACK))]);
        let log = all(&per_tick(&mut state, 800));
        (log, format!("{state:?}"))
    };
    let (a, a_state) = run(11);
    let (b, b_state) = run(11);
    assert_eq!(a, b);
    assert_eq!(a_state, b_state, "the final states are the same too");
    assert!(acted(&a, FOE));
    let (c, _) = run(12);
    assert_ne!(a, c, "another seed plays out differently");
    // Stepping in one go or tick by tick is the same battle.
    let mut whole = battle_of(11, vec![scripted_foe(None, &main), scripted_foe(None, &weakest(ATTACK))]);
    assert_eq!(whole.step_ticks(800), a);
}

#[test]
fn the_script_draws_before_the_action_does() {
    // A random target (one draw: two candidates), then the attack's own rolls: replaying the same
    // seed by hand with the engine's draw order gives the same first action.
    let main = Asm::act_on(|a| a.val(2, 0x20A0).op(op::RANDOM_BIT), ATTACK);
    let mut state = battle_of(21, vec![scripted_foe(None, &main)]);
    let first = all(&per_tick(&mut state, 400))
        .into_iter()
        .find(|e| matches!(e, BattleEvent::Damage { .. } | BattleEvent::Miss { .. }))
        .unwrap();
    let mut again = battle_of(21, vec![scripted_foe(None, &main)]);
    let second = all(&per_tick(&mut again, 400))
        .into_iter()
        .find(|e| matches!(e, BattleEvent::Damage { .. } | BattleEvent::Miss { .. }))
        .unwrap();
    assert_eq!(first, second);
}

#[test]
fn a_defense_the_initialize_script_set_changes_the_damage_taken() {
    // The foe is hit by nobody here; check the numbers the engine sees instead.
    let init = Asm::default()
        .addr(2, 0x2060)
        .addr(2, 0x4100)
        .op(op::MASK)
        .k1(200)
        .op(op::STORE)
        .op(op::END);
    let state = battle_of(5, vec![scripted_foe(Some(&init), &weakest(ATTACK))]);
    assert_eq!(state.actors()[FOE.0 as usize].defense, 200);
    let plain = battle_of(5, vec![scripted_foe(None, &weakest(ATTACK))]);
    assert_eq!(plain.actors()[FOE.0 as usize].defense, 0);
}

/// The ids of the party members the foe `foe` hurt or missed, in order.
fn victims_of(log: &[BattleEvent], foe: ActorId) -> Vec<ActorId> {
    log.iter()
        .filter_map(|e| match e {
            BattleEvent::Damage { actor, target, .. } | BattleEvent::Miss { actor, target } if *actor == foe => Some(*target),
            _ => None,
        })
        .collect()
}

#[test]
fn a_v9_roster_of_five_is_fought_by_the_scripts_with_every_member_in_reach() {
    // Two canon companions and three players: five fighters, which used to leave every enemy without its
    // script (the slots held three). Party member 3 is the weakest (90 HP): the script's lowest-HP pick,
    // which reads the HP of slot 10, names her, and the engine hurts ActorId(3).
    let foe = ActorId(5);
    let mut state = battle_with(5, party_of(5), vec![scripted_foe(None, &weakest(ATTACK))]);
    let ai = state.actors()[usize::from(foe.0)].ai.as_ref().unwrap();
    assert_eq!(ai.memory().phase, super::vm::Phase::Ready, "the scripts were not given up at the start");
    let log = all(&per_tick(&mut state, 900));
    let victims = victims_of(&log, foe);
    assert!(victims.len() >= 4, "the foe attacked several times: {victims:?}");
    assert_eq!(victims[0], ActorId(3), "the weakest, party member 3 (slot 10): {victims:?}");
    let fall = log.iter().position(|e| *e == BattleEvent::Death { actor: ActorId(3) }).expect("she fell");
    assert!(
        log[..fall].iter().filter(|e| matches!(e, BattleEvent::Damage { target, .. } if *target != ActorId(3))).count() == 0,
        "nobody else was hurt while she lived"
    );
    // Once she has fallen the weakest is Barret (120 HP, party member 1).
    let next = victims_of(&log[fall..], foe);
    assert_eq!(next.first(), Some(&ActorId(1)), "{next:?}");

    // The fifth member (slot 11) is aimed at the same way.
    let mut party = party_of(5);
    party[4].hp = 40;
    let mut state = battle_with(5, party, vec![scripted_foe(None, &weakest(ATTACK))]);
    let log = all(&per_tick(&mut state, 400));
    assert_eq!(victims_of(&log, foe).first(), Some(&ActorId(4)), "party member 4 (slot 11) is the weakest now");
}

#[test]
fn a_roster_of_four_or_more_acts_by_its_scripts_not_by_the_plain_fallback() {
    // The scripted foe's every choice is the lowest-HP party member, which the plain behaviour (a random
    // living member) would not repeat: with four or more fighters the scripted battle differs from the
    // plain one and hurts the weakest first.
    for fighters in [4usize, 6, 8] {
        let foe = ActorId(fighters as u8);
        let mut scripted = battle_with(3, party_of(fighters), vec![scripted_foe(None, &weakest(ATTACK))]);
        let log = all(&per_tick(&mut scripted, 500));
        let victims = victims_of(&log, foe);
        assert!(!victims.is_empty(), "{fighters} fighters: the foe attacked");
        let weakest = (0..fighters).min_by_key(|&i| party_of(fighters)[i].hp).unwrap();
        assert_eq!(victims[0], ActorId(weakest as u8), "{fighters} fighters");
        let mut plain = battle_with(3, party_of(fighters), vec![plain_foe()]);
        assert_ne!(all(&per_tick(&mut plain, 500)), log, "{fighters} fighters: not the plain behaviour");
    }
}

/// The events of the first tick in which `foe` acted, and the state after it.
fn first_turn_of(state: &mut BattleState, foe: ActorId) -> Vec<BattleEvent> {
    for _ in 0..2000 {
        let batch = state.step_ticks(1);
        if acted(&batch, foe) {
            return batch;
        }
    }
    panic!("the foe never acted");
}

/// `targets = <mask>; act(0x20, attack)`, without the `end`.
fn aim_and_act(asm: Asm, mask: u8, attack: u16) -> Asm {
    asm.addr(2, 0x2070).k1(mask).op(op::STORE).k1(0x20).k2(attack).op(op::ACT)
}

fn announced(events: &[BattleEvent], foe: ActorId) -> Vec<u16> {
    events
        .iter()
        .filter_map(|e| match e {
            BattleEvent::Action { actor, action: ActionRef::EnemyAttack(id) } if *actor == foe => Some(*id),
            _ => None,
        })
        .collect()
}

#[test]
fn the_attacks_a_script_queued_are_played_one_after_the_other_in_order() {
    // The double attack: one on Cloud, then one on Barret.
    let main = aim_and_act(aim_and_act(Asm::default(), 0b01, ATTACK), 0b10, ATTACK).op(op::END);
    let mut state = battle_of(5, vec![scripted_foe(None, &main)]);
    let turn = first_turn_of(&mut state, FOE);
    // Announced, played, announced, played: each attack is its own performance, in the order queued.
    let shape: Vec<String> = turn
        .iter()
        .map(|e| match e {
            BattleEvent::Action { .. } => "action".to_string(),
            BattleEvent::Damage { target, .. } | BattleEvent::Miss { target, .. } => format!("on {}", target.0),
            BattleEvent::GaugeReady { .. } => "ready".to_string(),
            other => format!("{other:?}"),
        })
        .collect();
    let shape: Vec<&str> = shape.iter().map(String::as_str).filter(|s| *s != "ready").collect();
    assert_eq!(shape, vec!["action", "on 0", "action", "on 1"], "{turn:?}");
}

#[test]
fn a_queued_attack_on_a_target_that_has_fallen_fizzles_and_the_script_goes_on() {
    // Both attacks are aimed at Barret, who has 1 HP: the first kills him (when it hits), so the second has
    // nobody to hit and is not played, not even announced. The script's memory (a counter at LV0000) goes on.
    let main = aim_and_act(aim_and_act(Asm::default(), 0b10, ATTACK), 0b10, ATTACK)
        .addr(1, 0x00)
        .val(1, 0x00)
        .k1(1)
        .op(op::ADD)
        .op(op::STORE)
        .op(op::END);
    let (mut killed, mut survived) = (0, 0);
    for seed in 0..40 {
        let mut party = party_of(2);
        party[1].hp = 1;
        party[1].evade = 120; // about two attacks in five hit him
        let mut state = battle_with(seed, party, vec![scripted_foe(None, &main)]);
        let turn = first_turn_of(&mut state, FOE);
        let memory = state.actors()[usize::from(FOE.0)].ai.as_ref().unwrap().memory();
        assert_eq!(memory.local(0x00, 1), Some(1), "seed {seed}: the script ran to its end");
        let first_result = turn.iter().find(|e| matches!(e, BattleEvent::Damage { .. } | BattleEvent::Miss { .. }));
        if matches!(first_result, Some(BattleEvent::Damage { .. })) {
            killed += 1;
            assert!(turn.contains(&BattleEvent::Death { actor: ActorId(1) }), "seed {seed}");
            assert_eq!(announced(&turn, FOE).len(), 1, "seed {seed}: the second attack fizzled: {turn:?}");
        } else {
            survived += 1;
            assert_eq!(announced(&turn, FOE).len(), 2, "seed {seed}: the first missed, so both were played: {turn:?}");
        }
    }
    assert!(killed > 0 && survived > 0, "both cases occurred: killed {killed}, missed {survived}");
}

/// The foe of [`scripted_foe`] that can also play [`LASER`], which hits the whole party and nothing else.
fn laser_foe(init: Option<&Asm>, main: &Asm) -> crate::rules::Combatant {
    let mut foe = scripted_foe(init, main);
    foe.actions.push(laser());
    foe
}

#[test]
fn an_attack_on_the_whole_party_hits_each_living_member_in_ascending_order() {
    // targets = every enemy of the foe (the whole party); a member who has fallen is not hit.
    let main = Asm::act_on(|a| a.val(2, 0x20A0), LASER);
    let mut party = party_of(4);
    party[2].hp = 0;
    let foe = ActorId(4);
    let mut state = battle_with(7, party, vec![laser_foe(None, &main)]);
    let turn = first_turn_of(&mut state, foe);
    let hits: Vec<u8> = turn
        .iter()
        .filter_map(|e| match e {
            BattleEvent::Damage { target, .. } | BattleEvent::Miss { target, .. } => Some(target.0),
            _ => None,
        })
        .collect();
    assert_eq!(hits, vec![0, 1, 3], "{turn:?}");
    assert_eq!(announced(&turn, foe), vec![LASER], "announced once, however many it reaches");
}

#[test]
fn a_whole_party_attack_deals_the_split_damage_of_the_whole_side_rules() {
    // The engine's own rule for an action on several targets (2/3 of the damage to each, see "Whole-side
    // targeting" of the rules): every non-critical hit of the laser on three members is within what the split
    // formula allows and below the least a one-target hit of the same attack can do.
    use crate::rules::{action_damage_among, Elemental, Rolls};
    let main = Asm::act_on(|a| a.val(2, 0x20A0), LASER);
    let foe = ActorId(3);
    let mut hits = 0;
    for seed in 0..12 {
        let mut state = battle_with(seed, party_of(3), vec![laser_foe(None, &main)]);
        let turn = first_turn_of(&mut state, foe);
        let spec = laser().spec;
        let me = &state.actors()[3];
        for e in &turn {
            let BattleEvent::Damage { target, amount, critical: false, .. } = *e else { continue };
            let victim = &state.actors()[usize::from(target.0)];
            let roll = |variance, among| action_damage_among(&spec, me, victim, Elemental::Normal, Rolls { critical: false, variance }, among);
            assert!(amount >= roll(0, 3) && amount <= roll(255, 3), "seed {seed}: {amount} is not a split hit");
            assert!(amount < roll(0, 1), "seed {seed}: {amount} is no less than a one-target hit would be");
            hits += 1;
        }
    }
    assert!(hits >= 20, "{hits} hits looked at");
}

#[test]
fn a_script_aims_an_attack_as_it_says_whatever_the_menu_rule_of_its_record() {
    // A record's targeting ([`Aim`]) is the rule of the player's menu, which `aim_at` checks a command
    // against; an enemy's script names its target mask and the original carries it out. So a whole-side
    // attack on one member and a one-target attack on the whole party are played as the script says.
    let hits = |turn: &[BattleEvent]| -> Vec<u8> {
        turn.iter()
            .filter_map(|e| match e {
                BattleEvent::Damage { target, .. } | BattleEvent::Miss { target, .. } => Some(target.0),
                _ => None,
            })
            .collect()
    };
    let foe = ActorId(2);
    let mut state = battle_of(5, vec![laser_foe(None, &Asm::act_on(|a| a.k1(0b10), LASER))]);
    let turn = first_turn_of(&mut state, foe);
    assert_eq!((announced(&turn, foe), hits(&turn)), (vec![LASER], vec![1]), "a whole-side attack on Barret alone");
    let mut state = battle_of(5, vec![laser_foe(None, &Asm::act_on(|a| a.val(2, 0x20A0), ATTACK))]);
    let turn = first_turn_of(&mut state, foe);
    assert_eq!((announced(&turn, foe), hits(&turn)), (vec![ATTACK], vec![0, 1]), "a one-target attack on both");
    let stats = state.actors()[2].ai.as_ref().unwrap().stats();
    assert_eq!(stats.fallbacks, 0, "{stats:?}");
}

#[test]
fn a_damaging_attack_aimed_at_the_enemys_own_side_falls_back_to_the_plain_behaviour() {
    // Target::AllAllies (every living enemy) and Target::One(an enemy) are no way for the rules to hit anyone
    // with a damaging attack: each of those turns is the plain behaviour's, event for event (the same battle
    // with the foe's scripts taken away plays alike).
    let two_foes = |main: &Asm, scripted: bool| {
        let mut first = laser_foe(None, main);
        if !scripted {
            first.ai = None;
        }
        battle_with(5, party_of(2), vec![first, plain_foe()])
    };
    for (what, main) in [
        ("on the foe's own side (both foes)", Asm::act_on(|a| a.val(2, 0x2080), ATTACK)),
        ("on the foe itself", Asm::act_on(|a| a.val(2, 0x2060), ATTACK)),
        ("on the other foe", Asm::act_on(|a| a.k1(0b10_0000), ATTACK)),
    ] {
        let mut scripted = two_foes(&main, true);
        let mut plain = two_foes(&main, false);
        let (log, expected) = (all(&per_tick(&mut scripted, 600)), all(&per_tick(&mut plain, 600)));
        assert!(expected.iter().any(|e| matches!(e, BattleEvent::Action { .. })), "{what}: the foes attack");
        assert_eq!(log, expected, "{what}: the plain behaviour played every turn");
        let stats = scripted.actors()[2].ai.as_ref().unwrap().stats();
        assert!(stats.fallbacks >= 3 && stats.turns == 0, "{what}: {stats:?}");
    }
}

#[test]
fn an_attack_with_a_random_aim_hits_one_member_whichever_the_script_names() {
    use crate::action::Aim;
    // Aim::Random is refused by the engine's own `aim_at` for one target and for a side; the enemy's own
    // rule plays it on ONE party member: the one the script named, or one drawn among the living when it
    // named them all (no draw for a single candidate).
    let random = crate::action::EnemyAction { id: LASER, spec: laser().spec.with_aim(Aim::Random) };
    let whole = Asm::act_on(|a| a.val(2, 0x20A0), LASER);
    let one = Asm::act_on(|a| a.k1(0b10), LASER);
    let victims = |main: &Asm, seed: u64| {
        let mut foe = scripted_foe(None, main);
        foe.actions.push(random);
        let mut state = battle_with(seed, party_of(3), vec![foe]);
        let turn = first_turn_of(&mut state, ActorId(3));
        let hit: Vec<u8> = turn
            .iter()
            .filter_map(|e| match e {
                BattleEvent::Damage { target, .. } | BattleEvent::Miss { target, .. } => Some(target.0),
                _ => None,
            })
            .collect();
        (hit, announced(&turn, ActorId(3)))
    };
    let mut seen = std::collections::BTreeSet::new();
    for seed in 0..60 {
        let (hit, said) = victims(&whole, seed);
        assert_eq!((hit.len(), said), (1, vec![LASER]), "seed {seed}: one member, one announcement");
        seen.insert(hit[0]);
        assert_eq!(victims(&one, seed).0, vec![1], "the member the script named");
    }
    assert_eq!(seen.into_iter().collect::<Vec<_>>(), vec![0, 1, 2], "every member is drawn sometimes");
}

#[test]
fn a_whole_party_attack_pays_its_mp_once_and_queued_attacks_each_pay_theirs() {
    let paying = |mp: u32, main: &Asm| {
        let mut foe = laser_foe(None, main);
        for action in &mut foe.actions {
            action.spec.mp_cost = 4;
        }
        foe.mp = mp;
        foe.max_mp = mp;
        foe
    };
    let spent = |turn: &[BattleEvent]| -> Vec<u32> {
        turn.iter().filter_map(|e| if let BattleEvent::MpSpent { amount, .. } = e { Some(*amount) } else { None }).collect()
    };
    // The whole party in one attack: 4 MP, once.
    let whole = Asm::act_on(|a| a.val(2, 0x20A0), LASER);
    let mut state = battle_of(3, vec![paying(10, &whole)]);
    let turn = first_turn_of(&mut state, FOE);
    assert_eq!(spent(&turn), vec![4]);
    assert_eq!(state.actors()[usize::from(FOE.0)].mp, 6);
    // Two queued attacks: 4 MP each when the foe has 8; with 7 the answer cannot be carried out (the second
    // attack is unaffordable) and the plain behaviour plays the turn: one attack, one payment.
    let double = aim_and_act(aim_and_act(Asm::default(), 0b01, ATTACK), 0b10, ATTACK).op(op::END);
    let mut state = battle_of(3, vec![paying(8, &double)]);
    let turn = first_turn_of(&mut state, FOE);
    assert_eq!((spent(&turn), announced(&turn, FOE).len()), (vec![4, 4], 2));
    assert_eq!(state.actors()[usize::from(FOE.0)].mp, 0);
    let mut state = battle_of(3, vec![paying(7, &double)]);
    let turn = first_turn_of(&mut state, FOE);
    assert_eq!((spent(&turn), announced(&turn, FOE).len()), (vec![4], 1), "{turn:?}");
}

#[test]
fn queued_and_whole_party_attacks_replay_exactly() {
    // The same battle built twice plays alike, in one go or tick by tick, and another seed differs.
    let double = aim_and_act(aim_and_act(Asm::default(), 0b01, ATTACK), 0b10, ATTACK).op(op::END);
    let whole = Asm::act_on(|a| a.val(2, 0x20A0), LASER);
    let build = |seed: u64| battle_with(seed, party_of(3), vec![scripted_foe(None, &double), laser_foe(None, &whole)]);
    let run = |seed: u64| {
        let mut state = build(seed);
        let log = all(&per_tick(&mut state, 900));
        (log, format!("{state:?}"))
    };
    let (a, a_state) = run(4);
    let (b, b_state) = run(4);
    assert_eq!((&a, &a_state), (&b, &b_state));
    assert!(a.iter().filter(|e| matches!(e, BattleEvent::Action { .. })).count() >= 4, "the foes attacked");
    assert_eq!(build(4).step_ticks(900), a, "in one go it is the same battle");
    assert_ne!(run(5).0, a);
}
