//! Counter scripts inside a running battle: who answers which blow and in what order, what an answer that
//! cannot be followed leaves behind (nothing), and that a replay of the same inputs agrees exactly.

use crate::action::{ActionSpec, Spell};
use crate::rules::{ActionRef, ActorId, BattleEvent, BattleState, Combatant, Command, Outcome};

use super::asm::{battle_with, count_in, foe_with, party_of, plain_foe, scripted_foe, Asm, ATTACK, OTHER};
use super::script::{op, slot};
use super::vm::{Memory, Phase};

const CLOUD: ActorId = ActorId(0);
const BARRET: ActorId = ActorId(1);
/// The scripted foe's position in a battle of the two-member party.
const FOE: ActorId = ActorId(2);

/// Where each counter leaves its trace: the number of times it ran, in a byte of the enemy's memory.
const GENERAL_RAN: u16 = 0x00;
const PHYSICAL_RAN: u16 = 0x08;
const MAGICAL_RAN: u16 = 0x10;
const DEATH_RAN: u16 = 0x18;

/// Cloud, who can cast Fire (a magical blow that never misses) and Cure, and Barret, who has HP to spare:
/// the foe's answers must not be able to end the battle.
fn party() -> Vec<Combatant> {
    let mut party = party_of(2);
    party[0].spells = vec![
        Spell::new(27, ActionSpec::damage_spell(20, 255, 0, 4)),
        Spell::new(0, ActionSpec::cure_spell(10, 5)),
    ];
    party[0].mp = 50;
    party[0].max_mp = 50;
    for member in &mut party {
        member.hp = 5000;
        member.max_hp = 5000;
    }
    party
}

/// A Main script that passes the turn, so that the foe does nothing of its own.
fn pass() -> Asm {
    Asm::default().op(op::END)
}

/// `targets = me.<field>` (the slot mask of the last actor that hit it, which the engine noted) and an attack
/// on them.
fn strike_back(field: u16, attack: u16) -> Asm {
    Asm::default()
        .addr(2, 0x2070)
        .val(2, 0x2060)
        .val(2, field)
        .op(op::MASK)
        .op(op::STORE)
        .k1(0x20)
        .k2(attack)
        .op(op::ACT)
}

/// An attack the rules cannot play, aimed at the foe itself: it is announced and does nothing, so it shows
/// where in the order a script played.
fn announce(attack: u16) -> Asm {
    Asm::default().addr(2, 0x2070).val(2, 0x2060).op(op::STORE).k1(0x20).k2(attack).op(op::ACT)
}

fn wait_ready(state: &mut BattleState, who: ActorId) {
    for _ in 0..5000 {
        if state.ready_actors().contains(&who) {
            return;
        }
        state.step_ticks(1);
    }
    panic!("{who:?} never got its turn");
}

/// `who` gives `command` as soon as it can: the events of that command.
fn give(state: &mut BattleState, who: ActorId, command: Command) -> Vec<BattleEvent> {
    wait_ready(state, who);
    state.submit_command(who, command).expect("the engine accepts it")
}

fn memory(state: &BattleState, who: ActorId) -> &Memory {
    state.actors()[usize::from(who.0)].ai.as_ref().unwrap().memory()
}

fn ran(state: &BattleState, who: ActorId, trace: u16) -> u32 {
    memory(state, who).local(trace, 1).expect("a byte of memory")
}

/// A line per event with the numbers of the actors: `1:attack`, `1>2` (a blow or a miss from 1 on 2), `x2`
/// (2 fell), `0:magic27`, `0-4mp`, `2:enemy110` (an enemy attack by its id in hex).
fn shape(events: &[BattleEvent]) -> Vec<String> {
    events
        .iter()
        .map(|e| match e {
            BattleEvent::Action { actor, action } => {
                let what = match action {
                    ActionRef::Attack => "attack".to_string(),
                    ActionRef::Magic(i) => format!("magic{i}"),
                    ActionRef::Item(i) => format!("item{i}"),
                    ActionRef::Defend => "defend".to_string(),
                    ActionRef::Limit(i) => format!("limit{i}"),
                    ActionRef::Escape => "escape".to_string(),
                    ActionRef::EnemyAttack(id) => format!("enemy{id:x}"),
                };
                format!("{}:{what}", actor.0)
            }
            BattleEvent::Damage { actor, target, .. } | BattleEvent::Miss { actor, target } => format!("{}>{}", actor.0, target.0),
            BattleEvent::Death { actor } => format!("x{}", actor.0),
            BattleEvent::MpSpent { actor, amount } => format!("{}-{amount}mp", actor.0),
            other => format!("{other:?}"),
        })
        .collect()
}

fn attack_on(target: ActorId) -> Command {
    Command::Attack { target }
}

#[test]
fn a_general_counter_answers_whoever_struck_the_enemy() {
    let general = count_in(GENERAL_RAN).then(&strike_back(0x40D0, ATTACK)).op(op::END);
    let mut state = battle_with(3, party(), vec![foe_with(&[(slot::MAIN, &pass()), (slot::GENERAL_COUNTER, &general)])]);
    assert_eq!(memory(&state, FOE).phase, Phase::Ready);
    assert_eq!(memory(&state, FOE).last_attacker(), 0, "nobody has hit it yet");

    // Barret strikes: the blow, then (in the same batch) the foe's answer, on Barret.
    let events = give(&mut state, BARRET, attack_on(FOE));
    assert_eq!(shape(&events), vec!["1:attack", "1>2", "2:enemy110", "2>1"], "{events:?}");
    assert_eq!(memory(&state, FOE).last_attacker(), 1 << 1);
    assert_eq!(memory(&state, FOE).last_physical_attacker(), 1 << 1);
    assert_eq!(memory(&state, FOE).last_magical_attacker(), 0);
    assert_eq!(ran(&state, FOE, GENERAL_RAN), 1);

    // Cloud strikes next: this time the answer is on Cloud (it is whoever struck last).
    let events = give(&mut state, CLOUD, attack_on(FOE));
    assert_eq!(shape(&events), vec!["0:attack", "0>2", "2:enemy110", "2>0"], "{events:?}");
    assert_eq!(memory(&state, FOE).last_attacker(), 1);
    assert_eq!(ran(&state, FOE, GENERAL_RAN), 2);
    let stats = state.actors()[usize::from(FOE.0)].ai.as_ref().unwrap().stats();
    assert_eq!((stats.counters, stats.counters_skipped), (2, 0));
}

#[test]
fn the_physical_and_the_magical_counter_follow_the_general_one_by_the_kind_of_the_blow() {
    let general = count_in(GENERAL_RAN).then(&announce(OTHER)).op(op::END);
    let physical = count_in(PHYSICAL_RAN).then(&strike_back(0x40E0, ATTACK)).op(op::END);
    let magical = count_in(MAGICAL_RAN).then(&strike_back(0x40F0, ATTACK)).op(op::END);
    let death = count_in(DEATH_RAN).op(op::END);
    let foe = foe_with(&[
        (slot::MAIN, &pass()),
        (slot::GENERAL_COUNTER, &general),
        (slot::PHYSICAL_COUNTER, &physical),
        (slot::MAGICAL_COUNTER, &magical),
        (slot::DEATH_COUNTER, &death),
    ]);
    let mut state = battle_with(3, party(), vec![foe]);

    // A physical blow: the General counter (an announcement), then the Physical one (an attack on Barret).
    let events = give(&mut state, BARRET, attack_on(FOE));
    assert_eq!(shape(&events), vec!["1:attack", "1>2", "2:enemy111", "2:enemy110", "2>1"], "{events:?}");
    assert_eq!([GENERAL_RAN, PHYSICAL_RAN, MAGICAL_RAN, DEATH_RAN].map(|t| ran(&state, FOE, t)), [1, 1, 0, 0]);

    // A magical one: the General counter, then the Magical one, on Cloud.
    let events = give(&mut state, CLOUD, Command::Magic { spell: 27, target: FOE });
    assert_eq!(shape(&events), vec!["0:magic27", "0-4mp", "0>2", "2:enemy111", "2:enemy110", "2>0"], "{events:?}");
    assert_eq!([GENERAL_RAN, PHYSICAL_RAN, MAGICAL_RAN, DEATH_RAN].map(|t| ran(&state, FOE, t)), [2, 1, 1, 0]);
    // It remembers who hit it how: Barret by hand, Cloud by magic, and Cloud last of all.
    let m = memory(&state, FOE);
    assert_eq!((m.last_attacker(), m.last_physical_attacker(), m.last_magical_attacker()), (1, 1 << 1, 1));
}

#[test]
fn a_blow_that_kills_runs_the_death_counter_and_no_other() {
    let mut weak = foe_with(&[
        (slot::MAIN, &pass()),
        (slot::GENERAL_COUNTER, &count_in(GENERAL_RAN).op(op::END)),
        (slot::PHYSICAL_COUNTER, &count_in(PHYSICAL_RAN).op(op::END)),
        (slot::DEATH_COUNTER, &count_in(DEATH_RAN).then(&announce(OTHER)).op(op::END)),
    ]);
    weak.hp = 1;
    weak.max_hp = 1;
    // A second enemy keeps the battle going, so the dying one's last words are not cut off by the end of it.
    let mut state = battle_with(3, party(), vec![weak, plain_foe()]);
    let events = give(&mut state, CLOUD, attack_on(FOE));
    assert_eq!(shape(&events), vec!["0:attack", "0>2", "x2", "2:enemy111"], "{events:?}");
    assert_eq!(state.outcome(), None);
    assert_eq!([GENERAL_RAN, PHYSICAL_RAN, DEATH_RAN].map(|t| ran(&state, FOE, t)), [0, 0, 1], "only the Death counter ran");
    // It was told who killed it, whether or not it had a counter to say it with.
    assert_eq!(memory(&state, FOE).last_attacker(), 1);
}

#[test]
fn no_counter_runs_once_the_battle_is_over_but_the_enemy_still_noted_the_blow() {
    let mut weak = foe_with(&[(slot::MAIN, &pass()), (slot::DEATH_COUNTER, &count_in(DEATH_RAN).then(&announce(OTHER)).op(op::END))]);
    weak.hp = 1;
    weak.max_hp = 1;
    let mut state = battle_with(3, party(), vec![weak]);
    let events = give(&mut state, BARRET, attack_on(FOE));
    assert_eq!(events.last(), Some(&BattleEvent::Victory), "{events:?}");
    assert_eq!(shape(&events), vec!["1:attack", "1>2", "x2", "Victory"], "nothing after the end");
    assert_eq!(ran(&state, FOE, DEATH_RAN), 0);
    assert_eq!(memory(&state, FOE).last_attacker(), 1 << 1);
    assert_eq!(state.outcome(), Some(Outcome::Victory));
}

#[test]
fn healing_a_friend_and_enemy_actions_start_no_counter() {
    let general = count_in(GENERAL_RAN).then(&announce(OTHER)).op(op::END);
    let mut state = battle_with(3, party(), vec![foe_with(&[(slot::MAIN, &pass()), (slot::GENERAL_COUNTER, &general)])]);
    let events = give(&mut state, CLOUD, Command::Magic { spell: 0, target: BARRET });
    assert!(!shape(&events).iter().any(|s| s.contains("enemy")), "{events:?}");
    assert_eq!(ran(&state, FOE, GENERAL_RAN), 0);
    assert_eq!(memory(&state, FOE).last_attacker(), 0, "nobody struck it");
    // Defending is no blow either.
    let events = give(&mut state, BARRET, Command::Defend);
    assert_eq!(shape(&events), vec!["1:defend"]);
    assert_eq!(ran(&state, FOE, GENERAL_RAN), 0);
}

#[test]
fn a_counter_aimed_at_the_whole_party_hits_every_living_member_once_announced_once() {
    // The Guard Scorpion's counter: targets = every enemy of the foe, attack. Tifa is down, the rest are hit
    // in ascending order, after the blow that started it.
    let laser = count_in(GENERAL_RAN).then(&Asm::default().addr(2, 0x2070).val(2, 0x20A0).op(op::STORE).k1(0x20).k2(ATTACK).op(op::ACT)).op(op::END);
    let mut party = party_of(4);
    for member in &mut party {
        member.hp = 5000;
        member.max_hp = 5000;
    }
    party[2].hp = 0;
    let foe = ActorId(4);
    let mut state = battle_with(3, party, vec![foe_with(&[(slot::MAIN, &pass()), (slot::GENERAL_COUNTER, &laser)])]);
    let events = give(&mut state, BARRET, attack_on(foe));
    assert_eq!(shape(&events), vec!["1:attack", "1>4", "4:enemy110", "4>0", "4>1", "4>3"], "{events:?}");
    assert_eq!(ran(&state, foe, GENERAL_RAN), 1);
}

#[test]
fn a_counter_that_cannot_be_followed_leaves_no_trace() {
    // The same battle with and without a General counter that uses an opcode the machine does not have: the
    // events are the same, every one of them, over several blows (so the random stream agrees too), and the
    // enemy knows the counter was skipped.
    let broken = Asm::default().op(op::LINK).op(op::END);
    let run = |foe: Combatant| {
        let mut state = battle_with(9, party(), vec![foe]);
        let mut log = Vec::new();
        for who in [BARRET, CLOUD, BARRET, CLOUD] {
            log.extend(give(&mut state, who, attack_on(FOE)));
        }
        (log, state)
    };
    let (with, state) = run(foe_with(&[(slot::MAIN, &pass()), (slot::GENERAL_COUNTER, &broken)]));
    let (without, _) = run(foe_with(&[(slot::MAIN, &pass())]));
    assert_eq!(with, without);
    assert!(with.iter().filter(|e| matches!(e, BattleEvent::Damage { .. })).count() >= 4);
    let stats = state.actors()[usize::from(FOE.0)].ai.as_ref().unwrap().stats();
    assert_eq!((stats.counters, stats.counters_skipped), (0, 4));
    // An answer the rules cannot carry out is no answer either: an attack the scene does not define.
    let unknown = strike_back(0x40D0, 0x0999).op(op::END);
    let (cannot, state) = run(foe_with(&[(slot::MAIN, &pass()), (slot::GENERAL_COUNTER, &unknown)]));
    assert_eq!(cannot, without);
    assert_eq!(state.actors()[usize::from(FOE.0)].ai.as_ref().unwrap().stats().counters_skipped, 4);
}

#[test]
fn an_enemy_that_gave_its_scripts_up_has_no_counters_and_notes_nothing() {
    let general = count_in(GENERAL_RAN).then(&announce(OTHER)).op(op::END);
    // Its Initialize script cannot be followed: the scripts are off for good.
    let init = Asm::default().op(op::LINK).op(op::END);
    let mut foe = foe_with(&[(slot::INITIALIZE, &init), (slot::MAIN, &pass()), (slot::GENERAL_COUNTER, &general)]);
    foe.dex = 0;
    let mut state = battle_with(3, party(), vec![foe]);
    assert_eq!(memory(&state, FOE).phase, Phase::Disabled);
    let events = give(&mut state, BARRET, attack_on(FOE));
    assert_eq!(shape(&events), vec!["1:attack", "1>2"], "{events:?}");
    assert_eq!(ran(&state, FOE, GENERAL_RAN), 0);
    assert_eq!(memory(&state, FOE).last_attacker(), 0);
}

#[test]
fn a_main_script_can_go_for_whoever_hit_the_enemy_last() {
    // Main: targets = me.last_attacker; attack. Before anyone has hit it that is nobody (the turn falls
    // back to the plain behaviour); afterwards it is Barret, every time, however the plain choice would go.
    let main = strike_back(0x40D0, ATTACK).op(op::END);
    let mut state = battle_with(11, party(), vec![scripted_foe(None, &main)]);
    let stats = |state: &BattleState| state.actors()[usize::from(FOE.0)].ai.as_ref().unwrap().stats();
    let events = give(&mut state, BARRET, attack_on(FOE));
    assert_eq!(shape(&events), vec!["1:attack", "1>2"], "a Main script is not a counter: it waits for its turn");
    assert!(stats(&state).fallbacks >= 1, "before the blow the foe fell back: {:?}", stats(&state));
    assert_eq!(stats(&state).turns, 0);
    let mut victims = Vec::new();
    for _ in 0..3000 {
        for e in state.step_ticks(1) {
            if let BattleEvent::Damage { actor, target, .. } | BattleEvent::Miss { actor, target } = e {
                if actor == FOE {
                    victims.push(target);
                }
            }
        }
        if victims.len() >= 3 {
            break;
        }
    }
    assert_eq!(victims, vec![BARRET; 3], "it goes for the one that hit it");
    assert!(stats(&state).turns >= 3, "{:?}", stats(&state));
}

// --- replay ---

/// What a server tells a client: the tick, the events, and the command it applied (if the message is one).
type Message = (u64, Vec<BattleEvent>, Option<(ActorId, Command)>);

/// Drive a battle like a server: advance in slices of `slice` ticks, announce what came of each slice, and let
/// every ready party member act (Cloud casts Fire while she has the MP, the others strike the first living
/// enemy). The log of messages and the final state.
fn serve(build: impl Fn() -> BattleState, slice: u64) -> (Vec<Message>, String) {
    let mut state = build();
    let mut log = Vec::new();
    for _ in 0..20_000 {
        if state.outcome().is_some() {
            break;
        }
        let events = state.step_ticks(slice);
        if !events.is_empty() {
            log.push((state.tick(), events, None));
        }
        for id in state.ready_actors() {
            let Some(target) = state.enemy_ids().into_iter().find(|e| state.actor(*e).is_some_and(Combatant::is_alive)) else {
                break;
            };
            let casts = id == CLOUD && state.actor(id).is_some_and(|c| c.mp >= 4);
            let command = if casts { Command::Magic { spell: 27, target } } else { attack_on(target) };
            if let Ok(events) = state.submit_command(id, command) {
                log.push((state.tick(), events, Some((id, command))));
            }
        }
    }
    (log, format!("{state:?}"))
}

/// What a client does with the messages (`BattleReplay::apply` of the protocol crate): advance to the tick the
/// message is for, check that the ticks of a command's message were silent, apply the command, and compare.
fn replay(build: impl Fn() -> BattleState, log: &[Message]) -> Result<String, String> {
    let mut state = build();
    for (tick, events, command) in log {
        let ahead = tick.checked_sub(state.tick()).ok_or("the server's tick went backwards")?;
        match command {
            None => {
                let got = state.step_ticks(ahead);
                if &got != events {
                    return Err(format!("tick {tick}: ours {got:?}, the server's {events:?}"));
                }
            }
            Some((actor, command)) => {
                let silent = state.step_ticks(ahead);
                if !silent.is_empty() {
                    return Err(format!("tick {tick}: ticks the server called silent produced {silent:?}"));
                }
                let got = state.submit_command(*actor, *command).map_err(|e| e.to_string())?;
                if &got != events {
                    return Err(format!("tick {tick}: ours {got:?}, the server's {events:?}"));
                }
            }
        }
    }
    Ok(format!("{state:?}"))
}

#[test]
fn counters_replay_exactly_from_the_commands_alone() {
    // Two foes whose counters draw from the battle's stream (a random target among the party) and whose Main
    // scripts do too; the party acts on its own. A client that has only the messages reproduces every event
    // and ends in the same state, whatever the slice the server advanced by.
    let random_answer = |counter: u16| count_in(counter).then(&Asm::act_on(|a| a.val(2, 0x20A0).op(op::RANDOM_BIT), ATTACK));
    let general = random_answer(GENERAL_RAN);
    let death = count_in(DEATH_RAN).then(&announce(OTHER)).op(op::END);
    let main = Asm::act_on(|a| a.val(2, 0x20A0).op(op::RANDOM_BIT), ATTACK);
    let seeded = |seed: u64| {
        let mut tough = foe_with(&[(slot::MAIN, &main), (slot::GENERAL_COUNTER, &general), (slot::DEATH_COUNTER, &death)]);
        tough.hp = 120;
        tough.max_hp = 120;
        tough.dex = 120;
        let mut weak = foe_with(&[(slot::MAIN, &main), (slot::GENERAL_COUNTER, &general), (slot::DEATH_COUNTER, &death)]);
        weak.hp = 60;
        weak.max_hp = 60;
        weak.dex = 90;
        battle_with(seed, party(), vec![tough, weak])
    };
    let build = || seeded(21);
    for slice in [1u64, 7, 50] {
        let (log, server_state) = serve(build, slice);
        let answers = log
            .iter()
            .filter(|(_, events, command)| {
                command.is_some() && events.iter().any(|e| matches!(e, BattleEvent::Action { actor, .. } if actor.0 >= 2))
            })
            .count();
        assert!(answers >= 3, "slice {slice}: the foes answered in {answers} commands");
        assert!(log.len() > 12, "slice {slice}: {} messages", log.len());
        assert_eq!(replay(build, &log).as_deref(), Ok(server_state.as_str()), "slice {slice}");
        // The same inputs again give the same battle: nothing here reads anything but the state and the stream.
        let (again, again_state) = serve(build, slice);
        assert_eq!((&again, &again_state), (&log, &server_state), "slice {slice}");
    }
    // A client built from another seed is another battle: it disagrees, loudly.
    let (log, _) = serve(build, 5);
    assert!(replay(|| seeded(22), &log).is_err(), "another seed is another battle");
}
