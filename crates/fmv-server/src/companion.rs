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
//!    HP, the companion casts the cheapest restoring spell it knows and can pay for on the most hurt one
//!    ([`Target::One`]: the lowest fraction of its HP, the lowest actor id on a tie). A spell that raises the dead never
//!    qualifies (the engine refuses it on someone who stands), and neither does one that can only be given to a whole
//!    side (the engine refuses it on one actor: the companions do not cast on a whole side).
//! 2. **Limit.** Else, if it has a limit break and the gauge is full, it gives it, aimed the way the limit's own
//!    [`Aim`] says (whole-side targeting, protocol v10): a damaging one at the first living enemy ([`Target::One`]), or
//!    at the whole enemy side ([`Target::AllEnemies`]) when that is the only way it is given; a healing one that takes
//!    one ally at the most hurt one, and one that heals the whole party (Healing Wind) at the party
//!    ([`Target::AllAllies`], companions and players alike), both only when somebody is hurt, so it is not wasted on a
//!    party that is fine.
//! 3. **Attack** the first living enemy.
//!
//! Every candidate is checked with `check_command` before it is chosen, so the command that comes out is one the
//! engine accepts right now (MP, targets and all); `None` only when there is nothing to do at all (no living enemy).
//! Defend, items and Escape are never chosen: the companions fight, and a companion that ran would end the
//! players' battle for them. A spell is never cast on a whole side (that costs an All materia use, and the rule
//! has no use for the choice); only a limit that can be given no other way is.
//!
//! # Pacing: the players go first
//!
//! The rule above says WHAT a companion does; [`may_act`] says WHEN. A companion that acted the instant its gauge filled
//! would end most fights of the first reactor before a player's turn came (md1stin's enemies die to a hit or two, and the
//! canon pair is as quick as any player), so a companion whose gauge is full HOLDS it while a living, connected player
//! still owes a turn: one that has given no more commands in this battle than the companion has. The players therefore
//! lead every round and the companions follow. Two bounds keep a fight from stalling on a player who does not act:
//!
//! * a player whose own gauge has been full for [`Pacing::turn_wait_ms`] (the time to choose a command in the menu that
//!   opens by itself) no longer holds anybody;
//! * a companion never holds its full gauge longer than [`Pacing::max_wait_ms`] (for a player whose gauge is slow to fill
//!   or never fills).
//!
//! Pacing only decides the tick at which the server submits a companion's command; nothing of it is part of what a replay
//! depends on (every client replays the command at the tick the server announces), so it needs no wire change.
//! `max_wait_ms == 0` turns it off: a companion then acts the instant its gauge fills, as in the first protocol-v9 build.

use fmv_ff7_battle::action::Aim;
use fmv_ff7_battle::limit::LimitInfo;
use fmv_ff7_battle::rules::{ActorId, BattleState, Combatant, Command, Target};

/// How long the canon companions let the players go first (see the module docs, "Pacing"). Battle time, in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pacing {
    /// How long a player whose gauge is full may take to choose before the companions stop waiting for it.
    pub turn_wait_ms: u32,
    /// The longest a companion holds a full gauge for the players. 0 turns pacing off (it acts as soon as it can).
    pub max_wait_ms: u32,
}

/// [`Pacing::turn_wait_ms`] by default: time to read the menu that opens on a player's turn and pick a command.
pub const DEFAULT_TURN_WAIT_MS: u32 = 5_000;
/// [`Pacing::max_wait_ms`] by default.
pub const DEFAULT_MAX_WAIT_MS: u32 = 10_000;

impl Default for Pacing {
    fn default() -> Self {
        Self { turn_wait_ms: DEFAULT_TURN_WAIT_MS, max_wait_ms: DEFAULT_MAX_WAIT_MS }
    }
}

impl Pacing {
    /// No pacing: a companion acts the instant its gauge fills.
    pub const OFF: Pacing = Pacing { turn_wait_ms: 0, max_wait_ms: 0 };
}

/// A living, connected player of the battle as [`may_act`] sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayerTurn {
    /// How many of its commands the engine has accepted in this battle.
    pub commands: u32,
    /// The engine tick its gauge has been full since (it is waiting for its player's command), `None` while it fills.
    pub ready_since: Option<u64>,
}

/// `ms` of battle time in engine ticks, rounded up (so a wait is never shorter than asked).
fn ticks(ms: u32, ticks_per_second: u32) -> u64 {
    (u64::from(ms) * u64::from(ticks_per_second.max(1))).div_ceil(1000)
}

/// Whether a canon companion whose gauge has been full since tick `ready_since`, and which has given `commands` commands
/// in this battle, may give its next one at tick `now` (module docs, "Pacing"). `players` are the battle's LIVING,
/// CONNECTED players (the caller leaves out the dead, who cannot act, and the disconnected, who never will); with none
/// left nobody is waited for.
pub fn may_act(pacing: Pacing, ticks_per_second: u32, now: u64, commands: u32, ready_since: u64, players: &[PlayerTurn]) -> bool {
    if now.saturating_sub(ready_since) >= ticks(pacing.max_wait_ms, ticks_per_second) {
        return true;
    }
    let turn_wait = ticks(pacing.turn_wait_ms, ticks_per_second);
    players
        .iter()
        .all(|p| p.commands > commands || p.ready_since.is_some_and(|since| now.saturating_sub(since) >= turn_wait))
}

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

/// Cast the cheapest restoring spell the actor can pay for on the most hurt member of the party (one target: a spell
/// that only ever hits a whole side fails the engine's check and is left out).
fn heal(state: &BattleState, actor: ActorId) -> Option<Command> {
    let target = Target::One(most_hurt(state)?);
    state
        .spells_for(actor)
        .into_iter()
        .filter(|s| s.targets_allies)
        .filter(|s| state.check_command(actor, &Command::Magic { spell: s.spell, target }).is_ok())
        .min_by_key(|s| (s.mp_cost, s.spell))
        .map(|s| Command::Magic { spell: s.spell, target })
}

/// Whom the limit break `info` is aimed at (what its [`Aim`] allows), or `None` when there is nobody worth it: a healing
/// one only while somebody is hurt (the party at once for one that can only be given that way, else the most hurt
/// one), a damaging one at the first living enemy (the whole enemy side only for one that can be given no other way).
/// A limit of [`Aim::Random`] is never aimed: neither a command nor a menu can ask for it.
fn limit_target(state: &BattleState, info: LimitInfo) -> Option<Target> {
    match (info.targets_allies, info.aim) {
        (_, Aim::Random) => None,
        (true, Aim::Side) => most_hurt(state).map(|_| Target::AllAllies),
        (true, Aim::One | Aim::OneOrSide) => most_hurt(state).map(Target::One),
        (false, Aim::Side) => first_enemy(state).map(|_| Target::AllEnemies),
        (false, Aim::One | Aim::OneOrSide) => first_enemy(state).map(Target::One),
    }
}

/// Give the limit break when the gauge is full and there is something worth aiming it at.
fn limit(state: &BattleState, actor: ActorId) -> Option<Command> {
    let info = state.limit_for(actor).filter(|_| state.limit_ready(actor))?;
    let command = Command::Limit { target: limit_target(state, info)? };
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

    // ---- pacing ----

    /// 60 ticks per second: 1 s is 60 ticks.
    const TPS: u32 = 60;
    const PACE: Pacing = Pacing { turn_wait_ms: 5_000, max_wait_ms: 10_000 };

    fn player(commands: u32, ready_since: Option<u64>) -> PlayerTurn {
        PlayerTurn { commands, ready_since }
    }

    #[test]
    fn a_companion_waits_for_every_player_that_still_owes_a_turn_this_round() {
        // Cloud's gauge filled at tick 100, before the player's: the player has not acted yet, so Cloud holds.
        assert!(!may_act(PACE, TPS, 101, 0, 100, &[player(0, None)]));
        // The player's gauge fills at 130 and its player is choosing: Cloud still holds.
        assert!(!may_act(PACE, TPS, 140, 0, 100, &[player(0, Some(130))]));
        // The player acted: Cloud follows at once.
        assert!(may_act(PACE, TPS, 141, 0, 100, &[player(1, None)]));
        // Next round: Cloud has acted once, so the player has to have acted twice before Cloud's second command.
        assert!(!may_act(PACE, TPS, 400, 1, 390, &[player(1, None)]));
        assert!(may_act(PACE, TPS, 401, 1, 390, &[player(2, None)]));
        // Two players: both have to have taken their turn.
        assert!(!may_act(PACE, TPS, 401, 0, 390, &[player(1, None), player(0, Some(395))]));
        assert!(may_act(PACE, TPS, 402, 0, 390, &[player(1, None), player(1, None)]));
    }

    #[test]
    fn a_player_who_does_not_choose_holds_nobody_after_the_turn_wait() {
        // The player's gauge has been full since tick 100 and it has not chosen: 5 s (300 ticks) later Cloud goes anyway.
        assert!(!may_act(PACE, TPS, 399, 0, 100, &[player(0, Some(100))]));
        assert!(may_act(PACE, TPS, 400, 0, 100, &[player(0, Some(100))]));
        // ... and in the next rounds too, without a new wait, for as long as that player's gauge stays full.
        assert!(may_act(PACE, TPS, 700, 1, 690, &[player(0, Some(100))]));
        // A turn wait counts from the player's own full gauge, not from the companion's.
        assert!(!may_act(PACE, TPS, 600, 0, 100, &[player(0, Some(400))]));
    }

    #[test]
    fn a_companion_never_holds_its_gauge_longer_than_the_cap() {
        // The player's gauge never fills (a very slow player, or one that cannot act): 10 s (600 ticks) is the limit.
        assert!(!may_act(PACE, TPS, 699, 0, 100, &[player(0, None)]));
        assert!(may_act(PACE, TPS, 700, 0, 100, &[player(0, None)]));
        // The waits are battle time: at 240 ticks per second the same 10 s are 2400 ticks.
        assert!(!may_act(PACE, 240, 100 + 2399, 0, 100, &[player(0, None)]));
        assert!(may_act(PACE, 240, 100 + 2400, 0, 100, &[player(0, None)]));
    }

    #[test]
    fn with_nobody_to_wait_for_or_pacing_off_a_companion_acts_at_once() {
        // Every player is dead or gone (the caller leaves them out).
        assert!(may_act(PACE, TPS, 100, 3, 100, &[]));
        // Off: the instant its gauge fills, whatever the players do.
        assert!(may_act(Pacing::OFF, TPS, 100, 0, 100, &[player(0, None)]));
        assert!(may_act(Pacing::OFF, TPS, 100, 5, 100, &[player(0, Some(100))]));
        // A turn wait of 0 alone releases a player the moment its gauge is full, but one still filling is waited for.
        let no_turn_wait = Pacing { turn_wait_ms: 0, ..PACE };
        assert!(may_act(no_turn_wait, TPS, 100, 0, 90, &[player(0, Some(100))]));
        assert!(!may_act(no_turn_wait, TPS, 100, 0, 90, &[player(0, None)]));
    }

    #[test]
    fn waits_are_rounded_up_to_whole_ticks() {
        assert_eq!(ticks(5_000, 60), 300);
        assert_eq!(ticks(10, 60), 1, "10 ms at 60/s is 0.6 ticks: one whole tick");
        assert_eq!(ticks(0, 60), 0);
        assert_eq!(ticks(1_000, 0), 1, "a zero rate is read as 1 per second");
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
        assert_eq!(command(&state, ActorId(0)), Some(Command::Magic { spell: 0, target: Target::One(ActorId(1)) }), "Cure, on Barret");
        // Barret heals himself too when he is the one who is hurt.
        assert_eq!(command(&state, ActorId(1)), Some(Command::Magic { spell: 0, target: Target::One(ActorId(1)) }));
        // Life (7) is never chosen: the engine refuses it on someone who stands.
        assert!(state.check_command(ActorId(0), &Command::Magic { spell: 7, target: Target::One(ActorId(1)) }).is_err());
        // Without the 5 MP Cure costs the companion attacks instead (Fire is no healing).
        let broke = battle(vec![fighter("Cloud", 200, 4), fighter("Barret", 60, 60)], vec![foe(100)]);
        assert_eq!(command(&broke, ActorId(0)), Some(Command::Attack { target: ActorId(2) }));
        // Exactly half is not hurt; one point under is.
        let half = battle(vec![fighter("Cloud", 200, 60), fighter("Barret", 100, 60)], vec![foe(100)]);
        assert_eq!(command(&half, ActorId(0)), Some(Command::Attack { target: ActorId(2) }));
        let under = battle(vec![fighter("Cloud", 200, 60), fighter("Barret", 99, 60)], vec![foe(100)]);
        assert_eq!(command(&under, ActorId(0)), Some(Command::Magic { spell: 0, target: Target::One(ActorId(1)) }));
        // A dead member is nobody's patient (a healing spell cannot help it).
        let dead = battle(vec![fighter("Cloud", 200, 60), fighter("Barret", 0, 60)], vec![foe(100)]);
        assert_eq!(command(&dead, ActorId(0)), Some(Command::Attack { target: ActorId(2) }));
    }

    #[test]
    fn a_tie_between_the_hurt_goes_to_the_lowest_actor_id() {
        let state = battle(vec![fighter("Cloud", 200, 60), fighter("Barret", 50, 60), fighter("Player", 50, 60)], vec![foe(100)]);
        assert_eq!(command(&state, ActorId(0)), Some(Command::Magic { spell: 0, target: Target::One(ActorId(1)) }));
    }

    #[test]
    fn it_gives_its_limit_when_the_gauge_is_full_and_there_is_a_target_worth_it() {
        let mut cloud = fighter("Cloud", 200, 60);
        cloud.limit = 255;
        let state = battle(vec![cloud.clone()], vec![foe(100), foe(100)]);
        assert!(state.limit_ready(ActorId(0)));
        assert_eq!(command(&state, ActorId(0)), Some(Command::Limit { target: Target::One(ActorId(1)) }), "a damaging limit goes at the first living enemy");
        // A gauge that is not full is an attack.
        cloud.limit = 254;
        let state = battle(vec![cloud], vec![foe(100)]);
        assert_eq!(command(&state, ActorId(0)), Some(Command::Attack { target: ActorId(1) }));
        // Healing beats the limit: a hurt member gets the Cure first, the limit waits.
        let mut cloud = fighter("Cloud", 200, 60);
        cloud.limit = 255;
        let state = battle(vec![cloud, fighter("Barret", 10, 60)], vec![foe(100)]);
        assert_eq!(command(&state, ActorId(0)), Some(Command::Magic { spell: 0, target: Target::One(ActorId(1)) }));
    }

    #[test]
    fn a_healing_limit_is_only_given_when_somebody_is_hurt() {
        let mut aerith = fighter("Aerith", 200, 0);
        aerith.limit = 255;
        aerith.limit_break = Some(LimitBreak::new(142, ActionSpec::limit_heal(16), 40));
        // Nobody is hurt: a healing limit is not wasted, she attacks.
        let state = battle(vec![aerith.clone(), fighter("Cloud", 200, 0)], vec![foe(100)]);
        assert_eq!(command(&state, ActorId(0)), Some(Command::Attack { target: ActorId(2) }));
        // Somebody is hurt and she has no MP to cast with: the limit heals (Healing Wind is the whole party).
        let state = battle(vec![aerith, fighter("Cloud", 20, 0)], vec![foe(100)]);
        assert_eq!(command(&state, ActorId(0)), Some(Command::Limit { target: Target::AllAllies }));
    }

    /// A fighter whose limit gauge is full and whose limit is `spec` (the one the test is about); it has no MP, so a
    /// hurt member is the limit's business and not a Cure's.
    fn with_limit(name: &str, hp: u32, spec: ActionSpec) -> Combatant {
        let mut c = fighter(name, hp, 0);
        c.limit = 255;
        c.limit_break = Some(LimitBreak::new(128, spec, 40));
        c
    }

    /// The limit's own [`Aim`] says how it is given (protocol v10): one target at the first living enemy, the whole
    /// enemy side only for a limit that can be given no other way (one that may be either is given to one).
    #[test]
    fn a_damaging_limit_is_aimed_the_way_its_aim_says() {
        let enemies = || vec![foe(100), foe(100)];
        let aimed = |aim: Aim, foes: Vec<Combatant>| {
            let state = battle(vec![with_limit("Cloud", 200, ActionSpec::limit_strike(48).with_aim(aim))], foes);
            command(&state, ActorId(0))
        };
        let (first, second) = (ActorId(1), ActorId(2));
        assert_eq!(aimed(Aim::One, enemies()), Some(Command::Limit { target: Target::One(first) }));
        assert_eq!(aimed(Aim::One, vec![foe(0), foe(100)]), Some(Command::Limit { target: Target::One(second) }), "the first one is down");
        assert_eq!(aimed(Aim::OneOrSide, enemies()), Some(Command::Limit { target: Target::One(first) }), "one target is the choice");
        assert_eq!(aimed(Aim::Side, enemies()), Some(Command::Limit { target: Target::AllEnemies }));
        assert_eq!(aimed(Aim::Side, vec![foe(0), foe(100)]), Some(Command::Limit { target: Target::AllEnemies }));
        // A random aim cannot be asked for: the companion attacks instead of giving a command the engine refuses.
        assert_eq!(aimed(Aim::Random, enemies()), Some(Command::Attack { target: first }));
        // With every enemy down there is nobody to aim at, whatever the aim.
        for aim in [Aim::One, Aim::OneOrSide, Aim::Side] {
            assert_eq!(aimed(aim, vec![foe(0), foe(0)]), None, "{aim:?}");
        }
    }

    /// A healing limit takes the most hurt one when it hits one ally and the whole party when that is the only way it is
    /// given, and in both cases only while somebody is hurt (under half).
    #[test]
    fn a_healing_limit_is_aimed_the_way_its_aim_says_and_only_while_somebody_is_hurt() {
        let party = |aim: Aim, hurt: u32| {
            vec![with_limit("Aerith", 200, ActionSpec::limit_heal(16).with_aim(aim)), fighter("Cloud", 200, 0), fighter("Player", hurt, 0)]
        };
        let given = |aim: Aim, hurt: u32| command(&battle(party(aim, hurt), vec![foe(100)]), ActorId(0));
        let (player, enemy) = (ActorId(2), ActorId(3));
        assert_eq!(given(Aim::Side, 60), Some(Command::Limit { target: Target::AllAllies }));
        assert_eq!(given(Aim::One, 60), Some(Command::Limit { target: Target::One(player) }), "the hurt player");
        assert_eq!(given(Aim::OneOrSide, 60), Some(Command::Limit { target: Target::One(player) }));
        // Exactly half is not hurt, so the limit waits and she attacks, whatever the aim.
        for aim in [Aim::Side, Aim::One, Aim::OneOrSide] {
            assert_eq!(given(aim, 100), Some(Command::Attack { target: enemy }), "{aim:?}");
        }
        // A random aim is never given.
        assert_eq!(given(Aim::Random, 60), Some(Command::Attack { target: enemy }));
    }

    /// The party side of a whole-party limit is the whole roster: companions and players alike are healed (v9 puts up to
    /// five fighters there), and a whole-enemy-side limit hits every living enemy and nobody of the party.
    #[test]
    fn a_whole_side_limit_hits_everybody_on_the_side_companions_and_players_alike() {
        use fmv_ff7_battle::rules::BattleEvent;
        // Aerith and Cloud are the companions here, two players follow; the last player is the one who is hurt.
        let party = vec![
            with_limit("Aerith", 200, ActionSpec::limit_heal(16)),
            fighter("Cloud", 40, 0),
            fighter("Player 1", 200, 0),
            fighter("Player 2", 30, 0),
        ];
        let mut state = battle(party, vec![foe(100)]);
        let given = command(&state, ActorId(0)).expect("somebody is hurt");
        assert_eq!(given, Command::Limit { target: Target::AllAllies });
        let events = state.submit_command(ActorId(0), given).expect("accepted");
        let mut healed: Vec<u8> = events.iter().filter_map(|e| if let BattleEvent::Heal { target, .. } = e { Some(target.0) } else { None }).collect();
        healed.sort_unstable();
        assert_eq!(healed, [0, 1, 2, 3], "every member of the party side, the players too: {events:?}");

        let party = vec![with_limit("Cloud", 200, ActionSpec::limit_strike(48).with_aim(Aim::Side)), fighter("Player", 200, 0)];
        let mut state = battle(party, vec![foe(900), foe(900), foe(0)]);
        let given = command(&state, ActorId(0)).expect("two enemies stand");
        assert_eq!(given, Command::Limit { target: Target::AllEnemies });
        let events = state.submit_command(ActorId(0), given).expect("accepted");
        let mut hit: Vec<u8> = events.iter().filter_map(|e| if let BattleEvent::Damage { target, .. } = e { Some(target.0) } else { None }).collect();
        hit.sort_unstable();
        assert_eq!(hit, [2, 3], "the two living enemies and nobody else (not the fallen third, not the party): {events:?}");
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

    /// The same, for every way a limit can be aimed: whatever the limit's aim and kind, every command the rule gives in
    /// every state of a played battle is one the engine accepts, and the limit really is given (so the whole-side
    /// shapes are exercised, not only the attack the rule falls back to).
    #[test]
    fn every_limit_it_gives_is_accepted_by_the_engine_whatever_the_limit_aims_at() {
        // (what it is, its spec, whether the rule aims it at a whole side)
        let limits = [
            ("strike", ActionSpec::limit_strike(48), false),
            ("strike on either", ActionSpec::limit_strike(48).with_aim(Aim::OneOrSide), false),
            ("strike on every enemy", ActionSpec::limit_strike(48).with_aim(Aim::Side), true),
            ("heal one", ActionSpec::limit_heal(16).with_aim(Aim::One), false),
            ("heal the party", ActionSpec::limit_heal(16), true),
        ];
        for (name, spec, whole) in limits {
            let (mut given, mut whole_side) = (0, 0);
            for seed in 0..12u64 {
                // Two companions with the limit and a player behind them, against two enemies that hit often and hard
                // enough to leave somebody under half of its HP long before the fight is over (a healing limit waits
                // for that).
                let party = vec![with_limit("Cloud", 200, spec), with_limit("Barret", 200, spec), fighter("Player", 200, 0)];
                let mut brutes = vec![foe(500), foe(500)];
                for brute in &mut brutes {
                    brute.attack = 70;
                    brute.dex = 90;
                }
                let config = BattleConfig { ticks_per_second: 1000, battle_speed: 0, ..BattleConfig::default() };
                let mut state = BattleState::with_config(seed, party, brutes, config);
                for _ in 0..20_000 {
                    if state.outcome().is_some() {
                        break;
                    }
                    state.step_ticks(7);
                    // Only the two companions are driven by the rule, as in a battle.
                    for actor in state.ready_actors().into_iter().filter(|a| a.0 < 2) {
                        let Some(c) = command(&state, actor) else { continue };
                        assert_eq!(state.check_command(actor, &c), Ok(()), "{name}, seed {seed}: {c:?} for {actor:?}");
                        if let Command::Limit { target } = c {
                            given += 1;
                            whole_side += usize::from(target.is_side());
                        }
                        state.submit_command(actor, c).expect("accepted");
                    }
                }
            }
            assert!(given > 0, "{name}: the limit was given at least once in 12 battles");
            assert_eq!(whole_side > 0, whole, "{name}: {whole_side} of {given} limits were aimed at a whole side");
        }
    }
}
