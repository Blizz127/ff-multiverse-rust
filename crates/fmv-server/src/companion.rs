//! What a canon companion does with its turn (protocol v9).
//!
//! A battle's party is the story's canon party followed by the players ([`fmv_protocol::Controller`]); the canon members
//! are commanded by the SERVER. When one's ATB gauge is full the server asks [`command`] for its move and applies it
//! like a player's, as an ordinary [`Command`]: checked by the engine (`BattleState::check_command`), submitted, and
//! announced in a `BattleEvent` with the command attached, so every client replays and verifies it with the very
//! engine code it already has. The engine has no AI for the party, and nothing here is part of what a replay depends
//! on: only the commands that come out of it are.
//!
//! # The rule
//!
//! A pure function of the engine's own state (no clock, no randomness, no memory between turns), so the same battle
//! always gives the same companion moves, and a bot-style one:
//!
//! 1. **Heal.** If a living member of the party (a player or another companion) is HURT, under half of its maximum
//!    HP, the companion casts the cheapest restoring spell it knows and can pay for on the most hurt one (the lowest
//!    fraction of its HP, the lowest actor id on a tie). A spell that raises the dead never qualifies (the engine
//!    refuses it on someone who stands).
//! 2. **Limit.** Else, if it has a limit break and the gauge is full, it gives it: a damaging one at the first living
//!    enemy; a healing one (Healing Wind) only when somebody is hurt, so it is not wasted on a party that is fine.
//! 3. **Attack** the first living enemy.
//!
//! Every candidate is checked with `check_command` before it is chosen, so the command that comes out is one the
//! engine accepts right now (MP, targets and all); `None` only when there is nothing to do at all (no living enemy).
//! Defend, items and Escape are never chosen: the companions fight, and a companion that ran would end the
//! players' battle for them.

use fmv_ff7_battle::rules::{ActorId, BattleState, Combatant, Command};

/// Whether `c` is hurt enough to be healed: under half of its maximum HP.
fn is_hurt(c: &Combatant) -> bool {
    u64::from(c.hp) * 2 < u64::from(c.max_hp)
}

/// The living member of the party that is hurt most (the lowest fraction of its HP, then the lowest actor id), if any
/// is hurt.
fn most_hurt(state: &BattleState) -> Option<ActorId> {
    state
        .party_ids()
        .into_iter()
        .filter_map(|id| state.actor(id).filter(|c| c.is_alive() && is_hurt(c)).map(|c| (id, c)))
        .min_by_key(|(id, c)| (u64::from(c.hp) * 1000 / u64::from(c.max_hp.max(1)), id.0))
        .map(|(id, _)| id)
}

/// The first living enemy.
fn first_enemy(state: &BattleState) -> Option<ActorId> {
    state.enemy_ids().into_iter().find(|id| state.actor(*id).is_some_and(Combatant::is_alive))
}

/// Cast the cheapest restoring spell the actor can pay for on the most hurt member of the party.
fn heal(state: &BattleState, actor: ActorId) -> Option<Command> {
    let target = most_hurt(state)?;
    state
        .spells_for(actor)
        .into_iter()
        .filter(|s| s.targets_allies)
        .filter(|s| state.check_command(actor, &Command::Magic { spell: s.spell, target }).is_ok())
        .min_by_key(|s| (s.mp_cost, s.spell))
        .map(|s| Command::Magic { spell: s.spell, target })
}

/// Give the limit break when the gauge is full and there is something worth aiming it at.
fn limit(state: &BattleState, actor: ActorId) -> Option<Command> {
    let info = state.limit_for(actor).filter(|_| state.limit_ready(actor))?;
    let target = if info.targets_allies { most_hurt(state)? } else { first_enemy(state)? };
    let command = Command::Limit { target };
    state.check_command(actor, &command).ok()?;
    Some(command)
}

/// Attack the first living enemy.
fn attack(state: &BattleState, actor: ActorId) -> Option<Command> {
    let command = Command::Attack { target: first_enemy(state)? };
    state.check_command(actor, &command).ok()?;
    Some(command)
}

/// The command `actor` (a canon companion whose gauge is full) gives now: heal, else limit, else attack (see the module
/// docs). `None` when it has nothing it can do (it is dead, its gauge is not full, there is no living enemy).
pub fn command(state: &BattleState, actor: ActorId) -> Option<Command> {
    if !state.actor(actor).is_some_and(Combatant::is_alive) || !state.ready_actors().contains(&actor) {
        return None;
    }
    heal(state, actor).or_else(|| limit(state, actor)).or_else(|| attack(state, actor))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmv_ff7_battle::action::ActionSpec;
    use fmv_ff7_battle::limit::LimitBreak;
    use fmv_ff7_battle::rules::{BattleConfig, StartMode};
    use fmv_protocol::SyntheticRules;

    /// A fighter with the synthetic spells (Cure 0 for 5 MP, Life 7 for 34, Fire 27 for 4) and a strike limit.
    fn fighter(name: &str, hp: u32, mp: u32) -> Combatant {
        let mut c = Combatant::new(name, 6, 200);
        c.hp = hp;
        c.mp = mp;
        c.max_mp = 60;
        c.dex = 40;
        c.attack = 50;
        c.spells = SyntheticRules::spells();
        c.limit_break = Some(LimitBreak::new(128, ActionSpec::limit_strike(48), 40));
        c
    }

    fn foe(hp: u32) -> Combatant {
        let mut e = Combatant::new("Foe", 3, 100);
        e.hp = hp;
        e.dex = 10;
        e
    }

    /// Everybody's gauge is full (a preemptive start), so every living party member is ready.
    fn battle(party: Vec<Combatant>, foes: Vec<Combatant>) -> BattleState {
        let config = BattleConfig { start: StartMode::Preemptive, ..BattleConfig::default() };
        let mut state = BattleState::with_config(3, party, foes, config);
        state.step_ticks(0);
        state
    }

    #[test]
    fn it_attacks_the_first_living_enemy() {
        let state = battle(vec![fighter("Cloud", 200, 60)], vec![foe(100), foe(100)]);
        let (first, second) = (ActorId(1), ActorId(2));
        assert_eq!(command(&state, ActorId(0)), Some(Command::Attack { target: first }));
        // With the first one down it is the second.
        let state = battle(vec![fighter("Cloud", 200, 60)], vec![foe(0), foe(100)]);
        assert_eq!(command(&state, ActorId(0)), Some(Command::Attack { target: second }));
    }

    #[test]
    fn it_heals_the_most_hurt_member_with_the_cheapest_restoring_spell_it_can_pay_for() {
        // Cloud (0) is fine, Barret (1) is at 30% and the player (2) at 40%: Barret is the most hurt.
        let state = battle(vec![fighter("Cloud", 200, 60), fighter("Barret", 60, 60), fighter("Player", 80, 60)], vec![foe(100)]);
        assert_eq!(command(&state, ActorId(0)), Some(Command::Magic { spell: 0, target: ActorId(1) }), "Cure, on Barret");
        // Barret heals himself too when he is the one who is hurt.
        assert_eq!(command(&state, ActorId(1)), Some(Command::Magic { spell: 0, target: ActorId(1) }));
        // Life (7) is never chosen: the engine refuses it on someone who stands.
        assert!(state.check_command(ActorId(0), &Command::Magic { spell: 7, target: ActorId(1) }).is_err());
        // Without the 5 MP Cure costs the companion attacks instead (Fire is no healing).
        let broke = battle(vec![fighter("Cloud", 200, 4), fighter("Barret", 60, 60)], vec![foe(100)]);
        assert_eq!(command(&broke, ActorId(0)), Some(Command::Attack { target: ActorId(2) }));
        // Exactly half is not hurt; one point under is.
        let half = battle(vec![fighter("Cloud", 200, 60), fighter("Barret", 100, 60)], vec![foe(100)]);
        assert_eq!(command(&half, ActorId(0)), Some(Command::Attack { target: ActorId(2) }));
        let under = battle(vec![fighter("Cloud", 200, 60), fighter("Barret", 99, 60)], vec![foe(100)]);
        assert_eq!(command(&under, ActorId(0)), Some(Command::Magic { spell: 0, target: ActorId(1) }));
        // A dead member is nobody's patient (a healing spell cannot help it).
        let dead = battle(vec![fighter("Cloud", 200, 60), fighter("Barret", 0, 60)], vec![foe(100)]);
        assert_eq!(command(&dead, ActorId(0)), Some(Command::Attack { target: ActorId(2) }));
    }

    #[test]
    fn a_tie_between_the_hurt_goes_to_the_lowest_actor_id() {
        let state = battle(vec![fighter("Cloud", 200, 60), fighter("Barret", 50, 60), fighter("Player", 50, 60)], vec![foe(100)]);
        assert_eq!(command(&state, ActorId(0)), Some(Command::Magic { spell: 0, target: ActorId(1) }));
    }

    #[test]
    fn it_gives_its_limit_when_the_gauge_is_full_and_there_is_a_target_worth_it() {
        let mut cloud = fighter("Cloud", 200, 60);
        cloud.limit = 255;
        let state = battle(vec![cloud.clone()], vec![foe(100), foe(100)]);
        assert!(state.limit_ready(ActorId(0)));
        assert_eq!(command(&state, ActorId(0)), Some(Command::Limit { target: ActorId(1) }), "a damaging limit goes at the first living enemy");
        // A gauge that is not full is an attack.
        cloud.limit = 254;
        let state = battle(vec![cloud], vec![foe(100)]);
        assert_eq!(command(&state, ActorId(0)), Some(Command::Attack { target: ActorId(1) }));
        // Healing beats the limit: a hurt member gets the Cure first, the limit waits.
        let mut cloud = fighter("Cloud", 200, 60);
        cloud.limit = 255;
        let state = battle(vec![cloud, fighter("Barret", 10, 60)], vec![foe(100)]);
        assert_eq!(command(&state, ActorId(0)), Some(Command::Magic { spell: 0, target: ActorId(1) }));
    }

    #[test]
    fn a_healing_limit_is_only_given_when_somebody_is_hurt() {
        let mut aerith = fighter("Aerith", 200, 0);
        aerith.limit = 255;
        aerith.limit_break = Some(LimitBreak::new(142, ActionSpec::limit_heal(16), 40));
        // Nobody is hurt: a healing limit is not wasted, she attacks.
        let state = battle(vec![aerith.clone(), fighter("Cloud", 200, 0)], vec![foe(100)]);
        assert_eq!(command(&state, ActorId(0)), Some(Command::Attack { target: ActorId(2) }));
        // Somebody is hurt and she has no MP to cast with: the limit heals.
        let state = battle(vec![aerith, fighter("Cloud", 20, 0)], vec![foe(100)]);
        assert_eq!(command(&state, ActorId(0)), Some(Command::Limit { target: ActorId(1) }));
    }

    #[test]
    fn it_has_nothing_to_do_when_it_is_dead_not_ready_or_there_is_nobody_to_hit() {
        let state = battle(vec![fighter("Cloud", 0, 60), fighter("Barret", 200, 60)], vec![foe(100)]);
        assert_eq!(command(&state, ActorId(0)), None, "dead");
        assert!(command(&state, ActorId(1)).is_some());
        assert_eq!(command(&state, ActorId(9)), None, "nobody");
        // A normal start: nobody's gauge is full yet.
        let config = BattleConfig { start: StartMode::Zero, ..BattleConfig::default() };
        let waiting = BattleState::with_config(3, vec![fighter("Cloud", 200, 60)], vec![foe(100)], config);
        assert_eq!(command(&waiting, ActorId(0)), None, "not ready");
        // Every enemy dead: the battle is over and there is no move.
        let over = battle(vec![fighter("Cloud", 200, 60)], vec![foe(0)]);
        assert_eq!(command(&over, ActorId(0)), None);
    }

    /// The same state gives the same move, and a move the engine accepts, in every state of many played battles:
    /// the companion is driven by `command` over whole fights against the synthetic enemies, the engine accepts each
    /// move it gives, and two runs agree to the event.
    #[test]
    fn it_is_deterministic_and_never_gives_a_command_the_engine_refuses() {
        let play = |seed: u64| {
            let party = vec![fighter("Cloud", 200, 60), fighter("Barret", 200, 60)];
            let enemies = vec![foe(400), foe(400)];
            let config = BattleConfig { ticks_per_second: 1000, battle_speed: 0, ..BattleConfig::default() };
            let mut state = BattleState::with_config(seed, party, enemies, config);
            let mut log = Vec::new();
            for _ in 0..20_000 {
                if state.outcome().is_some() {
                    break;
                }
                log.extend(state.step_ticks(7));
                for actor in state.ready_actors() {
                    if let Some(c) = command(&state, actor) {
                        assert_eq!(state.check_command(actor, &c), Ok(()), "{c:?} for {actor:?}");
                        log.extend(state.submit_command(actor, c).expect("accepted"));
                    }
                }
            }
            (state.outcome(), log)
        };
        for seed in 0..20 {
            let (outcome, log) = play(seed);
            assert!(outcome.is_some(), "seed {seed}: the battle ends");
            assert_eq!((outcome, log), play(seed), "seed {seed}: the same battle twice");
        }
    }
}
