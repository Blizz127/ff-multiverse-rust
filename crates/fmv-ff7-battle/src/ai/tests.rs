//! `choose` and `initialize` on invented battles and scripts.

use super::asm::{
    attacks, battle_of, battle_with, block, party, plain_foe, scripted_foe, Asm, ATTACK, OTHER,
};
use super::script::op;
use super::*;
use crate::rules::Combatant;

/// The foe's position in `battle_of`: Cloud and Barret are 0 and 1.
const FOE: usize = 2;

fn weakest(attack: u16) -> Asm {
    Asm::act_on(
        |a| a.val(2, 0x20A0).val(3, 0x4160).op(op::MASK).op(op::LEAST),
        attack,
    )
}

fn turn(state: &BattleState) -> Option<AiTurn> {
    choose(state, FOE, &mut Rng::new(1))
}

fn memory_of(state: &BattleState, idx: usize) -> &Memory {
    state.actors()[idx].ai.as_ref().unwrap().memory()
}

#[test]
fn an_enemy_without_scripts_is_not_scripted() {
    let state = battle_of(1, vec![plain_foe()]);
    assert!(turn(&state).is_none());
    assert!(initialize(&state, FOE, &mut Rng::new(1)).is_none());
    // Neither is a party member, or a position that is not in the battle.
    assert!(choose(&state, 0, &mut Rng::new(1)).is_none());
    assert!(choose(&state, 9, &mut Rng::new(1)).is_none());
}

#[test]
fn a_ready_enemy_attacks_where_its_main_script_says() {
    let state = battle_of(1, vec![scripted_foe(None, &weakest(ATTACK))]);
    assert_eq!(memory_of(&state, FOE).phase, Phase::Ready, "the battle initialized it");
    let got = turn(&state).unwrap();
    // The weakest of Cloud (300) and Barret (120) is Barret, position 1.
    assert_eq!(
        got.choice,
        Some(EnemyChoice {
            attack: ATTACK,
            target: Target::One(ActorId(1))
        })
    );
    assert!(got.changes.is_empty());
}

#[test]
fn a_random_target_depends_on_the_rng_passed_in_alone() {
    let main = Asm::act_on(|a| a.val(2, 0x20A0).op(op::RANDOM_BIT), ATTACK);
    let state = battle_of(1, vec![scripted_foe(None, &main)]);
    let mut seen = std::collections::BTreeSet::new();
    for seed in 0..64 {
        let a = choose(&state, FOE, &mut Rng::new(seed)).unwrap();
        let b = choose(&state, FOE, &mut Rng::new(seed)).unwrap();
        assert_eq!(a, b);
        seen.insert(a.choice.unwrap().target == Target::One(ActorId(0)));
    }
    assert_eq!(seen.len(), 2, "both party members are picked sometimes");
}

#[test]
fn a_script_that_cannot_be_followed_gives_no_turn() {
    for (what, main) in [
        ("unsupported opcode", Asm::default().op(op::LINK).op(op::END)),
        ("divide by zero", Asm::default().k1(1).k1(0).op(op::DIV).op(op::END)),
        ("endless loop", Asm::default().jump(op::JUMP, 0)),
        ("runs off the end", Asm::default().k1(1)),
        ("unmodelled field", Asm::act_on(|a| a.val(2, 0x20A0).val(2, 0x4068).op(op::MASK), ATTACK)),
        ("second action", {
            let one = Asm::default().k1(0x20).k2(ATTACK).op(op::ACT);
            Asm([one.0.clone(), one.0, vec![op::END]].concat())
        }),
    ] {
        let state = battle_of(1, vec![scripted_foe(None, &main)]);
        assert!(turn(&state).is_none(), "{what}");
    }
}

#[test]
fn an_enemy_with_no_main_script_or_scripts_the_battle_cannot_hold_has_no_turn() {
    let mut foe = scripted_foe(None, &weakest(ATTACK));
    foe.ai = Some(EnemyAi::new(
        Arc::new(EnemyScripts::parse_block(&[0xFF; script::HEADER_BYTES])),
        attacks(),
    ));
    let state = battle_of(1, vec![foe]);
    assert!(turn(&state).is_none());
    // Four party members do not fit the script slots: the enemy is disabled when the battle starts.
    let party: Vec<Combatant> = (0..4).map(|i| Combatant::new(format!("P{i}"), 5, 100)).collect();
    let state = BattleState::new(1, party, vec![scripted_foe(None, &weakest(ATTACK))]);
    assert!(choose(&state, 4, &mut Rng::new(1)).is_none());
    assert_eq!(memory_of(&state, 4).phase, Phase::Disabled);
}

#[test]
fn the_attack_must_be_one_the_scene_defines() {
    let state = battle_of(1, vec![scripted_foe(None, &weakest(0x0999))]);
    assert!(turn(&state).is_none(), "an attack the scene does not define");
    // One the scene defines is a choice even if the rules cannot play it (the engine announces it).
    let state = battle_of(1, vec![scripted_foe(None, &weakest(OTHER))]);
    assert_eq!(turn(&state).unwrap().choice.unwrap().attack, OTHER);
}

#[test]
fn a_script_that_ends_without_an_action_passes_the_turn() {
    let state = battle_of(1, vec![scripted_foe(None, &Asm::default().op(op::END))]);
    let got = turn(&state).unwrap();
    assert_eq!(got.choice, None);
}

#[test]
fn targets_must_be_living_actors_the_engine_can_name() {
    let aim = |mask: u8| Asm::act_on(move |a| a.k1(mask), ATTACK);
    let two_foes = |main: &Asm| vec![scripted_foe(None, main), plain_foe()];
    for (what, mask) in [
        ("nobody", 0u8),
        ("slot 3, the formation's own actor", 0b1000),
        ("a slot with no actor", 0b100),
        ("a party member and a foe at once", 0b01_0001),
    ] {
        let state = battle_of(1, two_foes(&aim(mask)));
        assert!(turn(&state).is_none(), "{what}");
    }
    // A dead party member (Barret) cannot be aimed at, alone or in a side.
    let mut with_dead_barret = party();
    with_dead_barret[1].hp = 0;
    let dead = battle_with(1, with_dead_barret, two_foes(&aim(0b10)));
    assert!(turn(&dead).is_none());
    // One living actor is a single target; the whole living side is "all"; from either side.
    let state = battle_of(1, two_foes(&aim(0b01)));
    assert_eq!(turn(&state).unwrap().choice.unwrap().target, Target::One(ActorId(0)));
    let state = battle_of(1, two_foes(&aim(0b11)));
    assert_eq!(turn(&state).unwrap().choice.unwrap().target, Target::AllEnemies);
    let state = battle_of(1, two_foes(&aim(0b11_0000)));
    assert_eq!(turn(&state).unwrap().choice.unwrap().target, Target::AllAllies);
    let state = battle_of(1, two_foes(&aim(0b10_0000)));
    assert_eq!(turn(&state).unwrap().choice.unwrap().target, Target::One(ActorId(3)));
}

#[test]
fn initialize_runs_when_the_battle_is_built_and_the_battle_keeps_what_it_did() {
    // Init: me.defense = 77; LV0040 (word) = 1.  Main: targets = LV0040.
    let init = Asm::default()
        .addr(2, 0x2060)
        .addr(2, 0x4100)
        .op(op::MASK)
        .k1(77)
        .op(op::STORE)
        .addr(2, 0x2060)
        .addr(2, 0x4110)
        .op(op::MASK)
        .k2(500)
        .op(op::STORE)
        .addr(2, 0x40)
        .k1(1)
        .op(op::STORE)
        .op(op::END);
    let main = Asm::act_on(|a| a.val(2, 0x40), ATTACK);
    let state = battle_of(1, vec![scripted_foe(Some(&init), &main)]);
    let foe = &state.actors()[FOE];
    assert_eq!(foe.defense, 77, "the script's defense is in force from the start");
    assert_eq!(foe.magic_defense, 500);
    let memory = memory_of(&state, FOE);
    assert_eq!(memory.phase, Phase::Ready);
    assert_eq!(memory.local(0x40, 2), Some(1), "what Init stored is what Main reads");
    assert_eq!(
        turn(&state).unwrap().choice.unwrap().target,
        Target::One(ActorId(0)),
        "Main read LV0040 = 1, Cloud"
    );
}

#[test]
fn a_failing_or_acting_initialize_script_disables_the_enemy() {
    for (what, init) in [
        ("unsupported", Asm::default().op(op::LINK).op(op::END)),
        ("acts", Asm::default().k1(0x20).k2(ATTACK).op(op::ACT).op(op::END)),
        ("writes someone else's field", Asm::default().k1(1).addr(2, 0x4100).op(op::MASK).k2(1).op(op::STORE).op(op::END)),
    ] {
        let state = battle_of(1, vec![scripted_foe(Some(&init), &weakest(ATTACK))]);
        assert_eq!(memory_of(&state, FOE).phase, Phase::Disabled, "{what}");
        assert!(turn(&state).is_none(), "{what}");
        assert_eq!(state.actors()[FOE].defense, 0, "{what}: nothing it wrote is kept");
    }
}

#[test]
fn initialize_called_directly_reports_what_it_would_do() {
    let init = Asm::default()
        .addr(2, 0x2060)
        .addr(2, 0x4100)
        .op(op::MASK)
        .k1(9)
        .op(op::STORE)
        .op(op::END);
    let state = battle_of(1, vec![scripted_foe(Some(&init), &weakest(ATTACK))]);
    let started = initialize(&state, FOE, &mut Rng::new(1)).unwrap();
    assert_eq!(started.memory.phase, Phase::Ready);
    assert_eq!(
        started.changes,
        vec![Change::Defense {
            actor: ActorId(FOE as u8),
            value: 9
        }]
    );
}

#[test]
fn a_block_with_both_scripts_keeps_each_in_its_slot() {
    let init = Asm::default().op(op::END);
    let main = weakest(ATTACK);
    let scripts = block(Some(&init), &main);
    assert_eq!(scripts.slots().collect::<Vec<_>>(), vec![script::slot::INITIALIZE, script::slot::MAIN]);
}
