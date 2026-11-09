//! Scripted enemies see the statuses of the battle: the status word `0x4000` of an actor is fed from the status
//! model of [`crate::status`] (Death and Near-death from HP, the statuses the rules play from the combatant's
//! state), and a script that branches on one of its bits acts on it. Invented scripts and battles only.

use super::asm::{battle_with, party, scripted_foe, Asm, ATTACK};
use super::script::op;
use super::vm::World;
use super::world::BattleWorld;
use crate::action::{ActionSpec, StatusEffect};
use crate::rules::{ActorId, BattleEvent, BattleState};
use crate::status::{Status, StatusState};

/// The foe's position in a battle of [`party`] (Cloud and Barret): the third actor, script slot 4.
const FOE: ActorId = ActorId(2);

/// Main: attack the party members that have `status` (a bit of `0x4000` of the first sixteen: Poison, Sleep,
/// Silence, Near-death, Haste, ...), if there are any, and pass the turn when there are none.
///
/// `targets = (enemies' status word & bit) != 0; if (targets) act(attack on targets)`
fn attack_those_with(status: Status) -> Asm {
    assert!(status.bit() <= 0xFFFF, "a word constant is all the assembler has");
    let mut main = Asm::default()
        .addr(2, 0x2070)
        .val(2, 0x20A0)
        .val(3, 0x4000)
        .op(op::MASK)
        .k2(status.bit() as u16)
        .op(op::AND)
        .k1(0)
        .op(op::NE)
        .op(op::STORE)
        .val(2, 0x2070);
    let skip = main.here();
    main = main.jump(op::JUMP_IF_ZERO, 0);
    main = main.k1(0x20).k2(ATTACK).op(op::ACT);
    let end = main.here();
    main = main.op(op::END);
    main.patch(skip, end);
    main
}

/// Cloud (300 HP) and Barret (120 HP), Barret starting with `statuses`, against a foe that runs `main`.
fn battle_with_barret(statuses: &[Status], main: &Asm) -> BattleState {
    let mut people = party();
    people[1].status = StatusState::starting(statuses.iter().copied().collect());
    battle_with(5, people, vec![scripted_foe(None, main)])
}

/// Who the foe hit in `ticks` ticks, and whether it acted at all.
fn foe_victims(state: &mut BattleState, ticks: u64) -> (Vec<ActorId>, bool) {
    let events: Vec<BattleEvent> = (0..ticks).flat_map(|_| state.step_ticks(1)).collect();
    let victims = events
        .iter()
        .filter_map(|e| match e {
            BattleEvent::Damage { actor, target, .. } if *actor == FOE => Some(*target),
            _ => None,
        })
        .collect();
    let acted = events.iter().any(|e| matches!(e, BattleEvent::Action { actor, .. } if *actor == FOE));
    (victims, acted)
}

#[test]
fn a_script_that_tests_a_status_bit_acts_on_the_party_members_that_have_it() {
    for status in [Status::Poison, Status::Silence, Status::Haste] {
        let main = attack_those_with(status);
        // Nobody has it: the script ran (the plain behaviour would have attacked someone) and passed.
        let mut state = battle_with_barret(&[], &main);
        let (victims, acted) = foe_victims(&mut state, 700);
        assert!(!acted && victims.is_empty(), "{status}: nobody to attack");
        // Barret has it: every blow of the foe is at Barret and nobody else.
        let mut state = battle_with_barret(&[status], &main);
        let (victims, acted) = foe_victims(&mut state, 150);
        assert!(acted && !victims.is_empty(), "{status}: the foe went for Barret");
        assert!(victims.iter().all(|v| *v == ActorId(1)), "{status}: {victims:?}");
    }
    // A bit the party member does not have is not the one that is tested.
    let mut state = battle_with_barret(&[Status::Silence], &attack_those_with(Status::Poison));
    assert_eq!(foe_victims(&mut state, 400), (Vec::new(), false));
}

#[test]
fn near_death_and_death_are_in_the_word_too() {
    // Cloud at a quarter of his HP or less is near death, and the script goes for him (not for Barret).
    let mut people = party();
    people[0].hp = 70;
    let main = attack_those_with(Status::NearDeath);
    let mut state = battle_with(5, people, vec![scripted_foe(None, &main)]);
    let (victims, acted) = foe_victims(&mut state, 100);
    assert!(acted && victims.iter().all(|v| *v == ActorId(0)), "{victims:?}");
    // Every field read of the word, for the party and for the foe, from the world the scripts see.
    let mut people = party();
    people[0].hp = 70;
    people[1].hp = 0;
    let mut barret_sick = people.clone();
    barret_sick[1].hp = 100;
    barret_sick[1].status = StatusState::starting([Status::Poison, Status::Haste, Status::Confusion].into_iter().collect());
    let attacks = super::asm::attacks();
    let read = |people: Vec<crate::rules::Combatant>, slot: u8, addr: u16, size: u8| {
        let state = battle_with(1, people, vec![scripted_foe(None, &Asm::default().op(op::END))]);
        BattleWorld::new(&state, 2, &attacks).unwrap().actor_field(slot, addr, size)
    };
    assert_eq!(read(people.clone(), 0, 0x4000, 3), Some(Status::NearDeath.bit()));
    assert_eq!(read(people.clone(), 1, 0x4000, 3), Some(Status::Death.bit()), "the fallen have Death and nothing else");
    assert_eq!(read(people.clone(), 4, 0x4000, 3), Some(0), "the foe is well");
    // Poison and Haste are there, Confusion (which the rules do not play) is not, and a single bit reads alone.
    assert_eq!(read(barret_sick.clone(), 1, 0x4000, 3), Some(Status::Poison.bit() | Status::Haste.bit()));
    assert_eq!(read(barret_sick.clone(), 1, 0x4000 + 3, 0), Some(1), "bit 3 is Poison");
    assert_eq!(read(barret_sick.clone(), 1, 0x4000 + 6, 0), Some(0), "bit 6 is Confusion");
    // The damage immunities are not statuses and are not modelled.
    assert!(!super::world::field_known(0x4028, 0) && !super::world::field_known(0x4029, 0));
}

#[test]
fn what_a_script_reads_follows_the_battle_as_statuses_come_and_go() {
    // The same Main in one battle: Barret is poisoned until a cure arrives, and then the foe has nobody to go for.
    let main = attack_those_with(Status::Poison);
    let mut state = battle_with_barret(&[Status::Poison], &main);
    let (before, _) = foe_victims(&mut state, 100);
    assert!(!before.is_empty() && before.iter().all(|v| *v == ActorId(1)));
    let antidote = ActionSpec::status_item(StatusEffect::cure(Status::Poison, 63), true);
    let mut events = Vec::new();
    state.resolve_action(0, &[1], &antidote, &mut events);
    assert_eq!(events, [BattleEvent::StatusEnded { target: ActorId(1), status: Status::Poison }]);
    let (after, acted) = foe_victims(&mut state, 400);
    assert!(!acted && after.is_empty(), "cured: nothing to go for ({after:?})");
}
