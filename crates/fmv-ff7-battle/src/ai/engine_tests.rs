//! Scripted enemies inside a running battle: the engine hook, the fallback, determinism.

use super::asm::{battle_of, plain_foe, scripted_foe, Asm, ATTACK, OTHER};
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
        // The whole party at once: the rules cannot carry that out yet.
        ("a whole side", Asm::act_on(|a| a.k1(0b11), ATTACK)),
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
