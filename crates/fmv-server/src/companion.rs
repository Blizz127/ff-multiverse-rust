//! What a canon companion does with its turn (protocol v9).
//!
//! A battle's party is the story's canon party followed by the players ([`fmv_protocol::Controller`]). **Who commands the
//! canon members** is the server's `--companions-player-commanded` switch (`BattleSettings::companions_player_commanded`; on
//! in the server binary): with it ON, as in FF7, a PLAYER commands them (`BattleMember::commander`: the party's leader, or the
//! only player of a solo battle), a full gauge waits for that player's command and nothing in this module runs for them. This
//! module is what the SERVER does with the canon members it commands: with the switch OFF (the behaviour of protocol v9 to
//! v16), and for a companion whose commander has left the battle. When such a one's ATB gauge is full the server asks
//! [`command`] for its move and applies it like a player's, as an ordinary [`Command`]: checked by the engine
//! (`BattleState::check_command`), submitted, and announced in a `BattleEvent` with the command attached, so every client
//! replays and verifies it with the very engine code it already has. The engine has no AI for the party, and nothing here
//! is part of what a replay depends on: only the commands that come out of it are.
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
//! # Pacing: each on its own clock, the players' first turn kept, one performance at a time
//!
//! The rule above says WHAT a companion does; [`may_act`] says WHEN. The answer is FF7's own: **a companion acts when its
//! OWN gauge is full**, at the pace its Dexterity and the battle speed give it ([`fmv_ff7_battle::rules`], "Starting turn
//! timers" and the gauge formulas), like every other fighter of the battle, enemies included. Two things hold it, and only
//! while they apply:
//!
//! * **The players' first turn.** The first reactor's enemies die to a blow or two, and the canon pair is as quick as any
//!   player, so a companion that took every turn on its own clock would end most fights of md1 before a player's gauge had
//!   even filled (without any pacing a solo player acted in only 44-87% of md1stin's fights, two players in 13-47% and three
//!   in 0-12%: `how_the_players_fare_beside_the_canon_party`). So a companion **never delivers the blow that ends the battle
//!   while a living, connected player has not had a
//!   turn yet**: it leaves the last enemy standing (it holds its command, with its gauge full, as a fighter waiting for the
//!   other's move would) until that player has given a command, which is released by [`Pacing::turn_wait_ms`] (the time to
//!   choose in the menu that opens by itself, counted from that player's OWN full gauge, so an idle player stalls nobody)
//!   and by [`Pacing::max_wait_ms`] (the longest a companion holds a full gauge for any reason). Only the finishing blow is
//!   held: everything else a companion does, a heal, a limit break, a blow that leaves the enemy standing, goes on its own
//!   clock, and the player's first turn needs only that one enemy to be there when its gauge fills. This replaces the
//!   first pacing, which held EVERY command of a companion until the players had given as many as it had: it gave the
//!   players the lead but it also released the whole party at the moment of the player's command, so that one attack of the
//!   player's could be followed in the same instant by both companions' blows and end any fight ("one attack causes
//!   victory"). The companions no longer wait for a command's count, only for the one first turn.
//! * **One performance on the stage at a time.** FF7 plays one action at a time (the queue; a fighter whose gauge fills during
//!   another's animation waits for it), and the client plays each message's performance from its arrival, so two commands
//!   of the same instant are drawn on top of each other. A companion therefore waits until [`ACTION_GAP_MS`] after the last
//!   command of ANY party member, a player's included (about the length of an Attack's performance on the client: its
//!   impact 0.5 s after it begins and 0.45 s of follow-through; 1 s at the default pace), and never longer than
//!   `max_wait_ms`. The gap is a length of BATTLE time ([`Clock::gap`]): it scales with the Battle Speed the way the game's own
//!   animation pause does (the qhimm thread below: 1, 3 and 5 seconds at the fastest, default and slowest speed) and not with
//!   the tick rate, so a battle run faster than the default (the smokes' `--battle-speed 0 --battle-tick-rate 240`) keeps the
//!   gap a sixth of a gauge, as at the default, instead of making it longer than a gauge and halving what the companions do.
//!   Where two companions are ready the one whose gauge has been full longest goes first (FF7's own order: the order the
//!   gauges filled). The enemies are not on this account and are not held for a performance: the engine plays an enemy's turn
//!   inside the step that fills its gauge (FF7's Active mode, see below), so the server cannot hold or space it, and a
//!   companion's blow that comes while an enemy's performance is still on the client's screen is drawn over it (a KNOWN GAP;
//!   the gap could count enemy turns too, which was not measured and would hold the companions back after every enemy turn);
//!   a player's command is never delayed.
//!
//! # Active, Recommended and Wait
//!
//! FF7's ATB has three modes (the qhimm thread "[FF7 PSX] ATB Recommended/Wait Pause Logic", which read the game's code,
//! <https://forums.qhimm.com/index.php?topic=28474.0>): in **Active** the gauges stop only for a summon; in **Recommended**
//! (the game's default) they also stop while an animation plays, never while a menu is open; **Wait** also stops them in
//! the Magic, Item and Summon menus and while a target is chosen, but the command menu itself never stops anything. A
//! shared battle cannot pause for one of several people (the others, the companions and the enemies share its clock, and the
//! server cannot see which menu a client has open), so it plays **Active**: every gauge runs while a player chooses, an
//! enemy acts the tick its gauge fills even with a menu open (verified over the real first reactor, see the
//! `battle_real` tests), and the whole of what Wait gives a player, time to choose, is given instead as the first-turn
//! rule above (the fight cannot end under a player's menu, for `turn_wait_ms`) and the starting timers (a player's first turn
//! comes within a few seconds, and nobody starts a battle behind).
//!
//! Pacing only decides the tick at which the server submits a companion's command; nothing of it is part of what a replay
//! depends on (every client replays the command at the tick the server announces), so it needs no wire change.
//! `max_wait_ms == 0` turns it all off: a companion then acts the instant its gauge fills, as in the first protocol-v9 build.

use fmv_ff7_battle::action::Aim;
use fmv_ff7_battle::limit::LimitInfo;
use fmv_ff7_battle::rules::{v_timer_increase, ActorId, BattleState, Combatant, Command, Outcome, Target, DEFAULT_BATTLE_SPEED, DEFAULT_TICKS_PER_SECOND};

/// How the canon companions let the players have a first turn (see the module docs, "Pacing"). Battle time, in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pacing {
    /// How long a player whose gauge is full may take to choose before a companion may end the battle without its first turn.
    pub turn_wait_ms: u32,
    /// The longest a companion holds a full gauge, for a player's first turn or a busy stage. 0 turns pacing off (it acts as soon
    /// as it can).
    pub max_wait_ms: u32,
}

/// [`Pacing::turn_wait_ms`] by default: time to read the menu that opens on a player's turn and pick a command.
pub const DEFAULT_TURN_WAIT_MS: u32 = 5_000;
/// [`Pacing::max_wait_ms`] by default.
pub const DEFAULT_MAX_WAIT_MS: u32 = 10_000;
/// How long after the last command of a party member the stage is busy and no companion starts another (module docs,
/// "Pacing"), in milliseconds at the DEFAULT pace (60 ticks a second, Battle Speed 128): an Attack's performance is its impact
/// (0.5 s) and its follow-through (0.45 s). It is a length of battle time, not of the clock on the wall: the gap is 60 ticks at the
/// default Battle Speed and shortens or lengthens with it like the game's own animation pause (1, 3 and 5 seconds at the fastest,
/// default and slowest speed, [`Clock::gap`]), whatever the tick rate, so a battle run faster than the default (the smokes'
/// `--battle-speed 0 --battle-tick-rate 240`) keeps the gap in proportion to a gauge instead of throttling the companions.
pub const ACTION_GAP_MS: u32 = 1_000;

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
    /// How many of its commands the engine has accepted in this battle (0: it has not had its first turn).
    pub commands: u32,
    /// The engine tick its gauge has been full since (it is waiting for its player's command), `None` while it fills.
    pub ready_since: Option<u64>,
}

/// How a battle's time maps to its engine ticks: the tick rate (the pacing's waits are milliseconds of battle time, which
/// the server runs in real time) and the Battle Speed (the stage gap follows it). [`Clock::of`] reads both from a battle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Clock {
    pub ticks_per_second: u32,
    /// FF7's Battle Speed, 0 fastest to 255 slowest.
    pub battle_speed: u8,
}

impl Clock {
    /// The clock of `state`'s battle.
    pub fn of(state: &BattleState) -> Self {
        let config = state.config();
        Self { ticks_per_second: config.ticks_per_second, battle_speed: config.battle_speed }
    }

    /// `ms` of battle time in engine ticks, rounded up (so a wait is never shorter than asked).
    pub fn ticks(self, ms: u32) -> u64 {
        (u64::from(ms) * u64::from(self.ticks_per_second.max(1))).div_ceil(1000)
    }

    /// The stage gap in engine ticks ([`ACTION_GAP_MS`]): its length in ticks at the default pace (60 ticks a second), scaled by how
    /// much faster or slower than the default Battle Speed's V-timer ([`v_timer_increase`]) this battle's gauges fill, so 60
    /// ticks at the default speed, about 20 at the fastest and 100 at the slowest, whatever the tick rate. Rounded up, and at
    /// least one tick.
    pub fn gap(self) -> u64 {
        let default_ticks = u64::from(ACTION_GAP_MS) * u64::from(DEFAULT_TICKS_PER_SECOND) / 1000;
        let (default_pace, pace) = (u64::from(v_timer_increase(DEFAULT_BATTLE_SPEED)), u64::from(v_timer_increase(self.battle_speed)));
        (default_ticks * default_pace).div_ceil(pace.max(1)).max(1)
    }
}

/// Whether a canon companion whose gauge has been full since tick `ready_since` may give its command at tick `now` (module
/// docs, "Pacing"): not while the stage is busy (`last_action` is the tick of the last command of any party member, `None`
/// before the first), and a command that `ends_battle` (asked only when it matters, so the caller can make it as costly as a
/// probe of the engine) not while a player in `players` has not had its first turn. `players` are the battle's LIVING,
/// CONNECTED players that can take a turn (the caller leaves out the dead, who cannot act, the disconnected, who never will,
/// and the asleep or stopped, who owe none); with none left nobody is waited for. A companion that has held its full gauge
/// for the pacing's `max_wait_ms` acts whatever the stage or the players do.
pub fn may_act(
    pacing: Pacing,
    clock: Clock,
    now: u64,
    ready_since: u64,
    last_action: Option<u64>,
    players: &[PlayerTurn],
    ends_battle: impl FnOnce() -> bool,
) -> bool {
    if now.saturating_sub(ready_since) >= clock.ticks(pacing.max_wait_ms) {
        return true;
    }
    if last_action.is_some_and(|at| now.saturating_sub(at) < clock.gap()) {
        return false;
    }
    let turn_wait = clock.ticks(pacing.turn_wait_ms);
    let owes_a_turn = players.iter().any(|p| p.commands == 0 && !p.ready_since.is_some_and(|since| now.saturating_sub(since) >= turn_wait));
    !owes_a_turn || !ends_battle()
}

/// A canon companion whose gauge is full, as [`next_move`] sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ready {
    pub actor: ActorId,
    /// The engine tick its gauge has been full since.
    pub since: u64,
}

/// The command a canon companion gives NOW, with how many ticks it held its full gauge for it, if one may: of the `ready`
/// companions the one whose gauge has been full the longest (the lower actor on a tie: FF7's own order, the order the
/// gauges filled) that has something to do ([`command`]) and that [`may_act`] lets go at the engine's current tick. The one
/// definition of who goes next that the server and the tests that play a battle the way the server does share.
/// `last_action` is the tick of the last command of any party member and `players` the living, connected players that can
/// take a turn, as [`may_act`] says.
pub fn next_move(state: &BattleState, pacing: Pacing, last_action: Option<u64>, players: &[PlayerTurn], ready: &[Ready]) -> Option<(ActorId, Command, u64)> {
    let (now, clock) = (state.tick(), Clock::of(state));
    let mut order = ready.to_vec();
    order.sort_unstable_by_key(|r| (r.since, r.actor));
    order.into_iter().find_map(|r| {
        let given = command(state, r.actor)?;
        let probe = || ends_in_victory(state, r.actor, &given);
        may_act(pacing, clock, now, r.since, last_action, players, probe).then_some((r.actor, given, now.saturating_sub(r.since)))
    })
}

/// Whether `command` of `actor`, given now, would win the battle: the probe the first-turn rule asks about a companion's
/// command, run on a copy of the engine (the copy has the same generator, so the real command, given before anything else
/// happens, does exactly what the probe did). A command the engine refuses ends nothing.
pub fn ends_in_victory(state: &BattleState, actor: ActorId, command: &Command) -> bool {
    let mut probe = state.clone();
    probe.submit_command(actor, *command).is_ok() && probe.outcome() == Some(Outcome::Victory)
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
    const TPS: Clock = Clock { ticks_per_second: 60, battle_speed: DEFAULT_BATTLE_SPEED };
    const PACE: Pacing = Pacing { turn_wait_ms: 5_000, max_wait_ms: 10_000 };

    fn player(commands: u32, ready_since: Option<u64>) -> PlayerTurn {
        PlayerTurn { commands, ready_since }
    }

    /// `may_act` for a companion whose gauge has been full since `ready_since`, asked at `now`, with the last action at
    /// `last` and a command that does (`ends`) or does not end the battle.
    fn may(pacing: Pacing, clock: Clock, now: u64, ready_since: u64, last: Option<u64>, players: &[PlayerTurn], ends: bool) -> bool {
        may_act(pacing, clock, now, ready_since, last, players, || ends)
    }

    #[test]
    fn a_companion_acts_on_its_own_clock_whatever_the_players_have_given() {
        // A blow that leaves the enemy standing is never held for a player's turn: not while the player's gauge is still
        // filling, not while it chooses, and the count of anybody's commands is nobody's business.
        for players in [&[player(0, None)][..], &[player(0, Some(130))], &[player(1, None)], &[player(0, None), player(0, Some(100))]] {
            assert!(may(PACE, TPS, 101, 100, None, players, false), "{players:?}");
        }
        // No player at all (all dead or gone) or none that owes a turn: even the blow that ends the battle goes.
        assert!(may(PACE, TPS, 101, 100, None, &[], true));
        assert!(may(PACE, TPS, 101, 100, None, &[player(1, None), player(4, Some(30))], true));
    }

    #[test]
    fn the_blow_that_ends_the_battle_waits_for_every_players_first_turn() {
        // The player's gauge is still filling, or full and it is choosing: the last enemy is left for it.
        assert!(!may(PACE, TPS, 101, 100, None, &[player(0, None)], true));
        assert!(!may(PACE, TPS, 140, 100, None, &[player(0, Some(130))], true));
        // It has given a command (its first turn is behind it): the companion finishes the fight.
        assert!(may(PACE, TPS, 141, 100, None, &[player(1, None)], true));
        // Two players: both have to have had their first turn.
        assert!(!may(PACE, TPS, 401, 390, None, &[player(1, None), player(0, Some(395))], true));
        assert!(may(PACE, TPS, 402, 390, None, &[player(1, None), player(2, None)], true));
    }

    #[test]
    fn a_player_who_does_not_choose_holds_nobody_after_the_turn_wait() {
        // The player's gauge has been full since tick 100 and it has not chosen: 5 s (300 ticks) later the companion goes
        // anyway.
        assert!(!may(PACE, TPS, 399, 100, None, &[player(0, Some(100))], true));
        assert!(may(PACE, TPS, 400, 100, None, &[player(0, Some(100))], true));
        // ... and later ones too, without a new wait, for as long as that player's gauge stays full.
        assert!(may(PACE, TPS, 700, 690, None, &[player(0, Some(100))], true));
        // A turn wait counts from the player's own full gauge, not from the companion's.
        assert!(!may(PACE, TPS, 600, 100, None, &[player(0, Some(400))], true));
    }

    #[test]
    fn a_companion_never_holds_its_gauge_longer_than_the_cap() {
        // The player's gauge never fills (a very slow player, or one that cannot act): 10 s (600 ticks) is the limit.
        assert!(!may(PACE, TPS, 699, 100, None, &[player(0, None)], true));
        assert!(may(PACE, TPS, 700, 100, None, &[player(0, None)], true));
        // The waits are battle time: at 240 ticks per second the same 10 s are 2400 ticks.
        let fast = Clock { ticks_per_second: 240, battle_speed: DEFAULT_BATTLE_SPEED };
        assert!(!may(PACE, fast, 100 + 2399, 100, None, &[player(0, None)], true));
        assert!(may(PACE, fast, 100 + 2400, 100, None, &[player(0, None)], true));
    }

    #[test]
    fn one_performance_is_on_the_stage_at_a_time_and_a_busy_stage_cannot_starve_a_companion() {
        // The last command (anybody's of the party) was at tick 100: for 1 s (60 ticks at the default pace) no companion starts
        // another, whatever it is.
        assert!(!may(PACE, TPS, 100, 90, Some(100), &[], false));
        assert!(!may(PACE, TPS, 159, 90, Some(100), &[], false));
        assert!(may(PACE, TPS, 160, 90, Some(100), &[], false));
        // The gap is a length of battle time, so it is the same number of ticks at any tick rate (a faster rate runs the whole
        // battle faster) and follows the Battle Speed like the game's own animation pause: 20, 60 and 102 ticks at the
        // fastest, default and slowest speed (the pause the Qhimm thread measured is 1, 3 and 5 seconds).
        for ticks_per_second in [60, 240, 1000] {
            let at = |battle_speed| Clock { ticks_per_second, battle_speed };
            assert_eq!((at(0).gap(), at(DEFAULT_BATTLE_SPEED).gap(), at(255).gap()), (20, 60, 102), "{ticks_per_second} ticks a second");
        }
        let fastest = Clock { ticks_per_second: 240, battle_speed: 0 };
        assert!(!may(PACE, fastest, 100 + 19, 90, Some(100), &[], false));
        assert!(may(PACE, fastest, 100 + 20, 90, Some(100), &[], false));
        let slowest = Clock { ticks_per_second: 60, battle_speed: 255 };
        assert!(!may(PACE, slowest, 100 + 101, 90, Some(100), &[], false));
        assert!(may(PACE, slowest, 100 + 102, 90, Some(100), &[], false));
        // A stage that is busy again and again still lets a companion through once it has held its gauge for the cap.
        assert!(!may(PACE, TPS, 689, 90, Some(650), &[], false));
        assert!(may(PACE, TPS, 690, 90, Some(650), &[], false));
        // Before the first action there is no performance to wait for.
        assert!(may(PACE, TPS, 0, 0, None, &[], false));
    }

    #[test]
    fn with_pacing_off_a_companion_acts_at_once_whatever_happens() {
        for players in [&[][..], &[player(0, None)], &[player(0, Some(100))]] {
            assert!(may(Pacing::OFF, TPS, 100, 100, Some(100), players, true), "{players:?}");
        }
        // A turn wait of 0 alone releases a player the moment its gauge is full, but one still filling is waited for.
        let no_turn_wait = Pacing { turn_wait_ms: 0, ..PACE };
        assert!(may(no_turn_wait, TPS, 100, 90, None, &[player(0, Some(100))], true));
        assert!(!may(no_turn_wait, TPS, 100, 90, None, &[player(0, None)], true));
    }

    #[test]
    fn the_probe_of_a_blow_is_asked_only_when_a_player_owes_a_turn_and_the_stage_is_free() {
        let asked = std::cell::Cell::new(0);
        let probe = || {
            asked.set(asked.get() + 1);
            true
        };
        // Nobody owes a turn, the cap is up or the stage is busy: the answer does not depend on the engine.
        assert!(may_act(PACE, TPS, 101, 100, None, &[], probe));
        assert!(may_act(PACE, TPS, 101, 100, None, &[player(1, None)], probe));
        assert!(may_act(PACE, TPS, 700, 100, None, &[player(0, None)], probe));
        assert!(!may_act(PACE, TPS, 101, 100, Some(100), &[player(0, None)], probe));
        assert_eq!(asked.get(), 0, "the engine is not copied for nothing");
        // A player owes its first turn and the stage is free: now it matters.
        assert!(!may_act(PACE, TPS, 101, 100, None, &[player(0, None)], probe));
        assert_eq!(asked.get(), 1);
    }

    #[test]
    fn waits_are_rounded_up_to_whole_ticks() {
        let at = |ticks_per_second| Clock { ticks_per_second, battle_speed: DEFAULT_BATTLE_SPEED };
        assert_eq!(at(60).ticks(5_000), 300);
        assert_eq!(at(60).ticks(10), 1, "10 ms at 60/s is 0.6 ticks: one whole tick");
        assert_eq!(at(60).ticks(0), 0);
        assert_eq!(at(0).ticks(1_000), 1, "a zero rate is read as 1 per second");
        assert_eq!(at(60).gap(), 60);
    }

    #[test]
    fn next_move_gives_the_longest_waiting_companion_its_turn_and_the_stage_and_the_first_turn_apply_to_it() {
        let party = || vec![fighter("Cloud", 200, 60), fighter("Barret", 200, 60), fighter("Player", 200, 60)];
        let (cloud, barret) = (ActorId(0), ActorId(1));
        let ready = |cloud_since, barret_since| [Ready { actor: cloud, since: cloud_since }, Ready { actor: barret, since: barret_since }];
        let go = |state: &BattleState, ready: &[Ready], last: Option<u64>, players: &[PlayerTurn]| {
            next_move(state, PACE, last, players, ready).map(|(actor, _, held)| (actor, held))
        };
        // The battle stands at tick 0. FF7 takes the turns in the order the gauges filled: whoever has been full the longest goes
        // first, whatever its place in the party, and the lower actor on a tie.
        let state = battle(party(), vec![foe(100_000)]);
        assert_eq!(go(&state, &ready(9, 5), None, &[]), Some((barret, 0)));
        assert_eq!(go(&state, &ready(5, 9), None, &[]), Some((cloud, 0)));
        assert_eq!(go(&state, &ready(7, 7), None, &[]), Some((cloud, 0)));
        // A performance on the stage holds them both; the held ticks are what the log says.
        assert_eq!(go(&state, &ready(0, 0), Some(0), &[]), None);
        // A player that owes its first turn does not hold a blow that leaves the enemy standing ...
        assert_eq!(go(&state, &ready(0, 0), None, &[player(0, None)]), Some((cloud, 0)));
        // ... but does hold the one that ends the battle (an enemy with one HP dies to any blow that hits), until it has had it.
        let doomed = battle(party(), vec![foe(1)]);
        assert_eq!(go(&doomed, &ready(0, 0), None, &[player(0, None)]), None);
        assert_eq!(go(&doomed, &ready(0, 0), None, &[player(1, None)]), Some((cloud, 0)));
        assert_eq!(go(&doomed, &ready(0, 0), None, &[]), Some((cloud, 0)));
        // Nobody ready, nobody goes; a companion with nothing to do is passed over for the next.
        assert_eq!(go(&state, &[], None, &[]), None);
        let dead_enemies = battle(party(), vec![foe(0)]);
        assert_eq!(go(&dead_enemies, &ready(0, 0), None, &[]), None, "nothing to hit");
    }

    #[test]
    fn a_probe_says_whether_a_command_would_win_and_changes_nothing() {
        // Two foes: a blow on one of them leaves the other standing, whatever the blow does.
        let state = battle(vec![fighter("Cloud", 200, 60)], vec![foe(1), foe(100_000)]);
        let before = state.clone();
        assert!(!ends_in_victory(&state, ActorId(0), &Command::Attack { target: ActorId(1) }), "one enemy is left");
        assert!(!ends_in_victory(&state, ActorId(0), &Command::Attack { target: ActorId(2) }), "one enemy is left");
        assert_eq!(state, before, "the probe ran on a copy");
        // The last enemy standing: the probe agrees with what the real command then does, hit or miss.
        let state = battle(vec![fighter("Cloud", 200, 60)], vec![foe(0), foe(1)]);
        let attack = Command::Attack { target: ActorId(2) };
        let mut real = state.clone();
        real.submit_command(ActorId(0), attack).unwrap();
        assert_eq!(ends_in_victory(&state, ActorId(0), &attack), real.outcome() == Some(Outcome::Victory));
        // A command the engine refuses ends nothing.
        assert!(!ends_in_victory(&state, ActorId(0), &Command::Attack { target: ActorId(1) }), "the target is dead");
        assert!(!ends_in_victory(&state, ActorId(9), &attack), "nobody");
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
