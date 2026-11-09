//! `choose` and `initialize` on invented battles and scripts.

use super::asm::{
    attacks, battle_of, battle_with, block, party, party_of, plain_foe, scripted_foe, Asm, ATTACK, OTHER,
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
        got.actions,
        vec![EnemyChoice {
            attack: ATTACK,
            target: Target::One(ActorId(1))
        }]
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
        seen.insert(a.actions[0].target == Target::One(ActorId(0)));
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
        ("unmodelled field", Asm::act_on(|a| a.val(2, 0x20A0).val(2, 0x4090).op(op::MASK), ATTACK)),
        ("too many actions", {
            let one = Asm::default().k1(0x20).k2(ATTACK).op(op::ACT);
            Asm([one.0.repeat(vm::MAX_ACTIONS + 1), vec![op::END]].concat())
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
    // More party members than the script slots have room for: the enemy is disabled when the battle starts.
    let too_many = world::MAX_PARTY + 1;
    let party: Vec<Combatant> = (0..too_many).map(|i| Combatant::new(format!("P{i}"), 5, 100)).collect();
    let state = BattleState::new(1, party, vec![scripted_foe(None, &weakest(ATTACK))]);
    assert!(choose(&state, too_many, &mut Rng::new(1)).is_none());
    assert_eq!(memory_of(&state, too_many).phase, Phase::Disabled);
}

#[test]
fn a_party_of_four_or_more_no_longer_costs_the_enemies_their_scripts() {
    // Protocol v9: two canon companions and the players' own characters, so four and five fighters are
    // ordinary. The script slots hold up to eight of them.
    for fighters in 3..=world::MAX_PARTY {
        let state = battle_with(1, party_of(fighters), vec![scripted_foe(None, &weakest(ATTACK))]);
        let foe = fighters;
        assert_eq!(memory_of(&state, foe).phase, Phase::Ready, "{fighters} fighters: the battle initialized it");
        let got = choose(&state, foe, &mut Rng::new(1)).unwrap_or_else(|| panic!("{fighters} fighters: no turn"));
        // The weakest of the party is the one the script names, wherever its slot is: Aerith (90 HP) is fourth.
        let weakest_hp = state.actors()[..fighters].iter().map(|c| c.hp).min().unwrap();
        let weakest_at = state.actors()[..fighters].iter().position(|c| c.hp == weakest_hp).unwrap();
        assert_eq!(
            got.actions,
            vec![EnemyChoice {
                attack: ATTACK,
                target: Target::One(ActorId(weakest_at as u8))
            }],
            "{fighters} fighters"
        );
    }
}

#[test]
fn a_script_aims_at_party_members_three_and_four_through_the_remapped_slots() {
    // Five fighters: members 3 (slot 10) and 4 (slot 11) are the lowest HP and the highest in turn.
    let mut party = party_of(5);
    assert_eq!((party[3].hp, party[4].hp), (90, 400));
    let lowest = battle_with(1, party.clone(), vec![scripted_foe(None, &weakest(ATTACK))]);
    assert_eq!(
        choose(&lowest, 5, &mut Rng::new(1)).unwrap().actions[0].target,
        Target::One(ActorId(3)),
        "the lowest HP is party member 3, slot 10"
    );
    party[2].hp = 1;
    party[3].hp = 500;
    party[4].hp = 40;
    party[2].max_hp = 500;
    let lowest = battle_with(1, party, vec![scripted_foe(None, &weakest(ATTACK))]);
    assert_eq!(
        choose(&lowest, 5, &mut Rng::new(1)).unwrap().actions[0].target,
        Target::One(ActorId(2)),
        "party member 2 (slot 2) is lower than 4 now"
    );
    // A random pick among the enemies reaches all five, the extra ones included.
    let main = Asm::act_on(|a| a.val(2, 0x20A0).op(op::RANDOM_BIT), ATTACK);
    let state = battle_with(1, party_of(5), vec![scripted_foe(None, &main)]);
    let mut seen = std::collections::BTreeSet::new();
    for seed in 0..200 {
        let got = choose(&state, 5, &mut Rng::new(seed)).unwrap();
        let Target::One(who) = got.actions[0].target else { panic!("one target") };
        seen.insert(who.0);
    }
    assert_eq!(seen.into_iter().collect::<Vec<_>>(), vec![0, 1, 2, 3, 4]);
    // The whole party at once is "everyone on the other side", extra members included.
    let all = Asm::act_on(|a| a.val(2, 0x20A0), ATTACK);
    let state = battle_with(1, party_of(5), vec![scripted_foe(None, &all)]);
    assert_eq!(choose(&state, 5, &mut Rng::new(1)).unwrap().actions[0].target, Target::AllEnemies);
}

#[test]
fn the_attack_must_be_one_the_scene_defines() {
    let state = battle_of(1, vec![scripted_foe(None, &weakest(0x0999))]);
    assert!(turn(&state).is_none(), "an attack the scene does not define");
    // One the scene defines is a choice even if the rules cannot play it (the engine announces it).
    let state = battle_of(1, vec![scripted_foe(None, &weakest(OTHER))]);
    assert_eq!(turn(&state).unwrap().actions[0].attack, OTHER);
}

#[test]
fn a_script_that_ends_without_an_action_passes_the_turn() {
    let state = battle_of(1, vec![scripted_foe(None, &Asm::default().op(op::END))]);
    let got = turn(&state).unwrap();
    assert_eq!(got.actions, vec![]);
}

#[test]
fn every_attack_a_script_queues_is_a_choice_in_the_order_queued() {
    // The real "double attack": the first attack on Cloud, then (the mask is stored again) one on Barret.
    let main = Asm::default()
        .addr(2, 0x2070)
        .k1(0b01)
        .op(op::STORE)
        .k1(0x20)
        .k2(ATTACK)
        .op(op::ACT)
        .addr(2, 0x2070)
        .k1(0b10)
        .op(op::STORE)
        .k1(0x20)
        .k2(OTHER)
        .op(op::ACT)
        .op(op::END);
    let state = battle_of(1, vec![scripted_foe(None, &main)]);
    assert_eq!(
        turn(&state).unwrap().actions,
        vec![
            EnemyChoice { attack: ATTACK, target: Target::One(ActorId(0)) },
            EnemyChoice { attack: OTHER, target: Target::One(ActorId(1)) },
        ]
    );
    // One of them that cannot be followed makes the whole turn unfollowable (nothing is half played).
    let bad_second = Asm::default()
        .addr(2, 0x2070)
        .k1(0b01)
        .op(op::STORE)
        .k1(0x20)
        .k2(ATTACK)
        .op(op::ACT)
        .k1(0x20)
        .k2(0x0999)
        .op(op::ACT)
        .op(op::END);
    let state = battle_of(1, vec![scripted_foe(None, &bad_second)]);
    assert!(turn(&state).is_none(), "the second attack is not one of the scene's");
    let bad_target = Asm::default()
        .addr(2, 0x2070)
        .k1(0b01)
        .op(op::STORE)
        .k1(0x20)
        .k2(ATTACK)
        .op(op::ACT)
        .addr(2, 0x2070)
        .k1(0b100)
        .op(op::STORE)
        .k1(0x20)
        .k2(ATTACK)
        .op(op::ACT)
        .op(op::END);
    let state = battle_of(1, vec![scripted_foe(None, &bad_target)]);
    assert!(turn(&state).is_none(), "the second names a slot with no actor");
}

#[test]
fn targets_must_be_actors_the_engine_can_name() {
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
    // A dead party member (Barret) alone is still the one target the script named: whether an attack can be
    // played on a dead actor is the engine's business (the attack may do nothing at all). In a side it is
    // not "the living side" any more.
    let mut with_dead_barret = party();
    with_dead_barret[1].hp = 0;
    let dead = battle_with(1, with_dead_barret.clone(), two_foes(&aim(0b10)));
    assert_eq!(turn(&dead).unwrap().actions[0].target, Target::One(ActorId(1)));
    let dead_in_a_side = battle_with(1, with_dead_barret, two_foes(&aim(0b11)));
    assert!(turn(&dead_in_a_side).is_none());
    // One living actor is a single target; the whole living side is "all"; from either side.
    let state = battle_of(1, two_foes(&aim(0b01)));
    assert_eq!(turn(&state).unwrap().actions[0].target, Target::One(ActorId(0)));
    let state = battle_of(1, two_foes(&aim(0b11)));
    assert_eq!(turn(&state).unwrap().actions[0].target, Target::AllEnemies);
    let state = battle_of(1, two_foes(&aim(0b11_0000)));
    assert_eq!(turn(&state).unwrap().actions[0].target, Target::AllAllies);
    let state = battle_of(1, two_foes(&aim(0b10_0000)));
    assert_eq!(turn(&state).unwrap().actions[0].target, Target::One(ActorId(3)));
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
        turn(&state).unwrap().actions[0].target,
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
fn an_initialize_script_may_change_the_attack_magic_and_evasion_of_its_enemy() {
    let set = |addr: u16, v: u8| Asm::default().addr(2, 0x2060).addr(1, addr).op(op::MASK).k1(v).op(op::STORE);
    let init = set(0x4078, 90).then(&set(0x4068, 77)).then(&set(0x4070, 33)).op(op::END);
    let state = battle_of(1, vec![scripted_foe(Some(&init), &weakest(ATTACK))]);
    let foe = &state.actors()[FOE];
    assert_eq!((foe.evade, foe.attack, foe.magic), (90, 77, 33), "the battle applies what Initialize wrote");
    assert_eq!(memory_of(&state, FOE).phase, Phase::Ready);
    let started = initialize(&state, FOE, &mut Rng::new(1)).unwrap();
    assert_eq!(
        started.changes,
        vec![
            Change::Evade { actor: ActorId(FOE as u8), value: 90 },
            Change::Attack { actor: ActorId(FOE as u8), value: 77 },
            Change::MagicAttack { actor: ActorId(FOE as u8), value: 33 },
        ]
    );
}

#[test]
fn an_initialize_script_finds_a_sibling_by_enemy_id_and_may_make_itself_immune_to_death() {
    // The pattern of the real Initialize scripts: LV0040 = the active actors whose "Index of actor" is 58
    // (is my sibling in the battle?), and me.immune_death = 1.
    let init = Asm::default()
        .addr(2, 0x40)
        .val(2, 0x2050)
        .val(2, 0x4120)
        .op(op::MASK)
        .k1(58)
        .op(op::EQ)
        .op(op::STORE)
        .addr(2, 0x2060)
        .addr(0, 0x402C)
        .op(op::MASK)
        .k1(1)
        .op(op::STORE)
        .op(op::END);
    let main = Asm::act_on(|a| a.val(2, 0x40), ATTACK);
    let with_id = |id: u16, init: Option<&Asm>| {
        let mut foe = scripted_foe(init, &main);
        foe.enemy_id = Some(id);
        foe
    };
    let mut sibling = plain_foe();
    sibling.enemy_id = Some(58);
    let state = battle_of(1, vec![with_id(57, Some(&init)), sibling]);
    let memory = memory_of(&state, FOE);
    assert_eq!(memory.phase, Phase::Ready, "Initialize ran to its end");
    assert!(memory.immune_death());
    assert_eq!(memory.local(0x40, 2), Some(1 << 5), "the sibling is the enemy in slot 5");
    // The script's Main then reads what Initialize found: an attack aimed at the sibling's slot.
    assert_eq!(
        turn(&state).unwrap().actions[0].target,
        Target::One(ActorId(3)),
        "the sibling is position 3 of the battle"
    );
    // Without a sibling (another enemy, a party member or an enemy with no id) it found nobody.
    for other in [Some(59), None] {
        let mut stranger = plain_foe();
        stranger.enemy_id = other;
        let state = battle_of(1, vec![with_id(57, Some(&init)), stranger]);
        assert_eq!(memory_of(&state, FOE).local(0x40, 2), Some(0), "enemy id {other:?}");
        assert_eq!(memory_of(&state, FOE).phase, Phase::Ready);
    }
    let alone = battle_of(1, vec![with_id(57, Some(&init))]);
    assert_eq!(memory_of(&alone, FOE).local(0x40, 2), Some(0));
}

#[test]
fn a_block_with_both_scripts_keeps_each_in_its_slot() {
    let init = Asm::default().op(op::END);
    let main = weakest(ATTACK);
    let scripts = block(Some(&init), &main);
    assert_eq!(scripts.slots().collect::<Vec<_>>(), vec![script::slot::INITIALIZE, script::slot::MAIN]);
}
