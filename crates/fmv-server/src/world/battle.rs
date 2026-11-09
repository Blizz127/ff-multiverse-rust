//! Battles in the world: starting them (on request, or from a random encounter roll), freezing the
//! players in them, stepping the engine, validating commands and ending them.
//!
//! # Authority
//!
//! The server owns the engine (`fmv_ff7_battle::rules::BattleState`, built through the server's
//! own `fmv_protocol::BattleRules`). Clients only ask: a command is applied only when it comes from
//! the session that controls that character and the engine itself accepts it (gauge full, actor
//! alive, target a living enemy); anything else is answered with `BattleRejected` to the sender
//! alone. Every operation on the engine is announced to every member as one `BattleEvent` message
//! (see `fmv_protocol::battle`, "The input log"), so a client that replays them holds the same
//! engine.
//!
//! # Rules of the field while a battle runs
//!
//! * A battle starts for the requester's whole party (the members that are in the requester's area,
//!   not between two areas, and in no battle yet) or for the requester alone. A party member who is
//!   in another area is not pulled in, and a player in a battle cannot travel.
//! * **The roster (protocol v9): the canon party first, then the players.** The story's own party
//!   (`AreaRuntime::story_party` of the party LEADER's progression: Cloud and Barret in the first reactor mission) goes
//!   first as server-controlled companions (`Controller::Companion`), each at the LEADER's level (the requester's when
//!   the leader is not online; `--battle-level` pins the level of the whole fight); then every player in the party's own
//!   order (the requester alone without a party), each as its own account's character, the stat curve its `slot` says,
//!   at its own account's level (the player's character progress, see `progress`). A player's character is ADDED to the
//!   canon party and nobody is put in a canon member's place: a solo player fights as [Cloud, Barret, player], two
//!   players as [Cloud, Barret, P1, P2]. Member `i` fights as `ActorId(i)`. Two accounts that play the same curve fight
//!   as two of it, and a player on Cloud's curve fights beside the canon Cloud.
//! * **Companions.** After every step of the engine (`step_one`) each canon companion whose ATB gauge is full gets one
//!   ordinary command from [`crate::companion::command`] (heal a hurt member, else the limit break, else attack the
//!   first living enemy: a pure function of the engine's own state), applied through the same engine call as a
//!   player's and announced to every member in a `BattleEvent` with the command attached, so every client replays and
//!   checks it like any other. **The players go first** ([`crate::companion::may_act`], `BattleSettings::companion_pacing`):
//!   a companion holds its full gauge while a living, connected player still owes a turn this round (has given no more
//!   commands than the companion), for at most `turn_wait_ms` after that player's own gauge filled and `max_wait_ms` in
//!   all, so the players lead every round and an idle one never stalls the fight. Pacing only moves the tick at which
//!   the server submits the command, so it is invisible to a replay. It is logged as
//!   `companion acted battle=.. actor=.. name=.. command=.. character=.. held_ms=..` and counted in the battle's
//!   `commands`. A companion has no account and no entity: it is never paid or saved, no player may
//!   command it (`NotYourActor`), it does not keep a battle alive (a battle with no connected PLAYER left is dropped)
//!   and the rewards (`reward_roster`) are split among the living PLAYERS only, exactly as before. Players have no limit
//!   break in v9, so only a companion's gauge moves, and it is never stored.
//! * A player in a battle is FROZEN: its `MoveIntent`s are ignored (not applied, not answered),
//!   so nobody else sees it move, and it takes no random-encounter steps.
//! * When the battle ends every member is back in the field where it stood. Its entity is marked
//!   changed, so the next tick's `EntityUpdate` tells it (and everyone) where that is.
//! * A player that disconnects stays in the engine as an uncontrolled character; a battle with no
//!   connected PLAYER left is dropped (the canon companions alone do not keep one alive).
//! * An item command is checked against the ACCOUNT's inventory by the server (the engine knows none): the
//!   engine's own checks come first (gauge, target, MP, spell and item known), then the count
//!   (`BattleRejection::OutOfItems`), and the item is taken only when the engine accepted the command: the
//!   player's mirror loses one, the saver is queued the change and the player is told its inventory
//!   (`ServerMsg::Inventory`) right after the `BattleEvent` that applied it.
//! * **Limits and escape (protocol v7).** A member starts with the limit gauge its account's character
//!   carries (`BattleMember::limit`, the mirror's `Progress::limit` inside the game's cap) and every
//!   member's end gauge is kept when the battle ends, whatever the outcome (`keep_limits`): queued for the
//!   saver as a set (`SavedChange::Limit`) and put in the mirror, only when it is not the one the battle
//!   started with, and logged (`limit kept`). A `Limit` command is checked by the engine alone (the
//!   character has one, its gauge is full, the target is on the right side: `NoLimit`, `LimitNotReady`,
//!   `InvalidTarget`); an `Escape` too (`CannotEscape` when the formation forbids it). A successful escape
//!   ends the battle with `Outcome::Escaped` for everyone: no rewards, no drops, the members are back in
//!   the field; a failed one only costs the turn. Both are logged as they happen (`limit used`,
//!   `escape tried`), and so are the other commands that are not a plain attack (`magic used`, `item used`,
//!   `defend used`), each as `key=value` with `battle`, `actor`, `account` and `character`: a smoke run reads
//!   them as the server's own record of what it applied.
//! * **`--debug-limit-full`** (`BattleSettings::debug_limit_full`, a debug flag that needs
//!   `--debug-encounters`): every member of every battle this server starts begins with a FULL limit gauge,
//!   whatever its account's is, so that a smoke run can force a Limit. It changes the gauge the battle STARTS
//!   with (`BattleMember::limit`, so the `battle started` line says `limits=[255, ..]` and every replay
//!   starts from it), and under it NO limit gauge is ever stored: a gauge that started from a faked full one
//!   is not the account's own, so `keep_limits` below skips the whole save (logged as `limit not kept ...
//!   reason=debug-limit-full`) and every account keeps the gauge it really has, even when a member spent
//!   its limit or was knocked out. A test run with this flag can therefore never overwrite a real account's
//!   gauge. The server logs the flag loudly when it starts.
//! * **`--debug-status LIST`** (`BattleSettings::debug_status`, a debug flag that needs `--debug-encounters`): every
//!   member of every battle this server starts, the canon companions and the players alike, begins with those statuses
//!   (a real battle starts clean, and the first mission's enemies inflict none, so without this a smoke run over real
//!   processes would never see one). It changes only what the battle STARTS with (`BattleMember::status`, which
//!   travels in `EncounterStart`, so every replay starts from it and the engine announces them with its first step):
//!   the `battle started` line says `statuses=[Poison+Silence, ..]` (one entry per fighter in actor order, `-` for
//!   none), nothing is stored, and the server logs the flag loudly when it starts. The engine's own refusals
//!   (`CannotAct` for a member that is asleep, stopped or paralysed, `Silenced` for a silenced one's spell) answer a
//!   command like any other, and a member that cannot take a turn owes none to the companions' pacing.
//!   Whatever the engine announces about statuses is logged as `status inflicted battle=.. actor=.. status=..` and
//!   `status ended battle=.. actor=.. status=..` (a Poison or Regen turn is only counted: the `battle ended` line ends with
//!   `status_inflicted=N status_ended=N status_ticks=N`).
//! * **Counters.** An enemy with scripts answers a blow with its counter scripts (`fmv_ff7_battle::ai`, "Counters"), inside
//!   the very command that provoked it: the enemy's attacks come in that command's `BattleEvent` right after the
//!   command's own. The server counts every enemy attack in the events of a COMMAND (`Battle::note_counters`: submitting a
//!   command never advances the clock, so an enemy that acts in its batch is answering it) and logs each as
//!   `counter battle=.. actor=.. attack=..`; the `battle ended` line ends with `counters=N`. A Death counter that the clock
//!   runs (an enemy killed by a turn of Poison) comes in a step's events and is not counted. `scripts/battle-smoke.sh --boss`
//!   reads these lines (the real Guard Scorpion's Tail Laser).
//! * **`--debug-bonus-exp N` and `--debug-bonus-limit N`** (`BattleSettings::debug_bonus_exp` /
//!   `debug_bonus_limit`, debug flags that need `--debug-encounters` and which `main` refuses to run on the default
//!   state directory, or together with `--debug-limit-full`): they exist so that a smoke run over real processes can
//!   make a NEW character level up and fill a limit gauge in one short session, and then prove both survive a real
//!   server restart (a win here pays about 16 of the ~250 experience a level costs, and these weak enemies rarely hurt
//!   anybody). Unlike `--debug-limit-full` they DO store what they make up: the experience bonus is added to each
//!   member that fought on at PAY time (`pay_members`: the mirror, the saver, the game's own level-up rule and the
//!   `battle reward` line, whose `exp=` is what was really paid and which says `debug_bonus_exp=N`), and the gauge
//!   kept at the end (`keep_limits`) is the one the battle left plus `N`, up to the cap (`limit kept ... debug_bonus_limit=N`).
//!   The `BattleEnd` that clients verify by replay is built before, so it carries only what the battle itself earned.
//!   Nothing else is written anywhere: no file, no field, no record says that a number was made up, which is why
//!   they only ever run against a throw-away state directory.
//! * A victory pays the members that fought on (`BattleEnd::rewards`, split by
//!   `fmv_protocol::split_rewards`) and drops items: the server rolls them after the outcome from the
//!   battle's seed (`BattleRules::roll_drops`, logged) and deals them to the living members
//!   (`fmv_protocol::assign_drops`, `BattleReward::items`). Each payment is added to the member's character progress with the
//!   game's level-up rule (`progress::apply_reward`): the member that is online gets its mirror updated
//!   and a `CharacterProgress` message right after `BattleEnd` (then an `Inventory` one if it was dropped
//!   items), and every payment, online or not (a
//!   member that left mid-battle fought on), is queued for the saver, which adds it to the stored
//!   record. The stored record is a separate one from the account's progression blob, which is also its
//!   story phase key (`accounts::login` builds `PhaseKey(progression.blob)`): battle results written
//!   into that would move the player to another instance. A defeat pays nothing.
//!
//! # Time
//!
//! Like everything in the world, stepping takes an explicit `Instant`; the engine consumes the
//! whole milliseconds since its last step (the engine's own accumulator turns them into ticks).
//!
//! # Random encounters
//!
//! In an area whose `AreaRuntime::encounters()` is a non-empty table (each loaded area has its own,
//! read when it is loaded), every `encounter_step_units` of horizontal distance a player moves in
//! it is one roll: it triggers with probability `rate / 1024` and then picks a formation by the
//! table's weights. APPROXIMATION: this is not FF7's own step
//! counter, only a rate knob over the same table. By default a step is the area's `max_speed`
//! (one roll per second of walking at full speed, so the table's rate 128 means roughly one battle
//! per eight seconds of full-speed walking); `random_encounters: false` switches the rolls off.
//!
//! The server's own default field, `md1stin` (section 6 of the field file, Steam PC build): the
//! first of its two 24-byte tables is the enabled one, rate 128, with formations 301 (weight 24,
//! two enemies), 302 (22, one enemy) and 303 (18, two enemies), weights adding up to 64; the second
//! table is disabled. A starting party (every account at the level its character starts at) wins 301 and
//! 303 every time, which is why `scripts/battle-smoke.sh` fights 301.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fmv_adapter_api::{AreaRuntime, EncounterTable};
use fmv_core::{AccountId, EntityId};
use fmv_ff7_battle::rng::Rng;
use fmv_ff7_battle::rules::{ActionRef, ActorId, BattleEvent, BattleState, Combatant, Command, CommandError, Outcome, Target, MAX_STEP_MS};
use fmv_ff7_battle::status::StatusSet;
use fmv_protocol::{
    assign_drops, clip_character_name, reward_roster, split_rewards, AppliedCommand, BattleId, BattleMember, BattleRejection, BattleReward,
    BattleRules, BattleSetup, ServerMsg, MAX_BATTLE_PARTY,
};

use super::story::StoryThen;
use super::{is_full, Outbound, World};
use crate::character::log_token;
use crate::companion;
use crate::config::BattleSettings;
use crate::inventory::Change;
use crate::progress;
use crate::saver::SaveJob;

/// Most random-encounter rolls one move can make (a move is bounded by the speed budget anyway).
const MAX_ROLLS_PER_MOVE: u32 = 8;

/// What became of one battle request or command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BattleReply {
    /// Applied (the resulting messages were already queued for the members).
    Done,
    /// Refused; tell the requester (`ServerMsg::BattleRejected`) and nobody else.
    Refused { battle: Option<BattleId>, reason: BattleRejection },
    /// The requester is no longer in the world.
    Gone,
}

fn refused(battle: Option<BattleId>, reason: BattleRejection) -> BattleReply {
    BattleReply::Refused { battle, reason }
}

/// A member of a running battle: a player's character or a canon companion.
struct Member {
    info: BattleMember,
    /// False once its player left the world (its character stays in the engine, uncontrolled). A companion is never
    /// connected: it has no session, the server commands it.
    connected: bool,
}

/// One running battle.
struct Battle {
    state: BattleState,
    /// `members[i]` fights as `ActorId(i)`.
    members: Vec<Member>,
    /// `commands[i]`: how many commands of `ActorId(i)` the engine accepted (logged when the battle
    /// ends, so a log shows that every member really fought).
    commands: Vec<u32>,
    /// `ready_since[i]`: the engine tick `ActorId(i)`'s gauge has been full since (it waits for a command), `None` while
    /// it fills or once it is dead. What the companions' pacing measures its waits from (`companion::may_act`).
    ready_since: Vec<Option<u64>>,
    /// The engine has consumed time up to here.
    clock: Instant,
    /// The formation (`scene * 4 + slot`) and the seed the battle was started with: what the drops of a
    /// win are rolled from (they are not part of the replayed battle).
    formation: u16,
    seed: u64,
    /// How many `StatusInflicted`, `StatusEnded` and `StatusTick` events the engine has announced (logged when the
    /// battle ends; the first two are also logged one by one, see [`Battle::note_statuses`]).
    status_counts: [u32; 3],
    /// The story step this battle is the price of, applied when it is won (a battle a story point starts first); `None`
    /// for an encounter.
    then: Option<StoryThen>,
    /// How many enemy attacks answered a command (see [`Battle::note_counters`]; logged when the battle ends, and one by one).
    counters: u32,
}

/// How a set of statuses reads in a log line: the names joined by `+`, or `-` for none (one token, so that a list of
/// them never splits a line).
fn status_word(set: StatusSet) -> String {
    if set.is_empty() {
        return "-".to_string();
    }
    set.iter().map(|s| s.name()).collect::<Vec<_>>().join("+")
}

impl Battle {
    /// Count the status events among `events` and say each inflicted or ended one as a log line:
    /// `status inflicted battle=.. actor=.. status=..` and `status ended ...` (a Poison or Regen turn is only counted).
    fn note_statuses(&mut self, id: BattleId, events: &[BattleEvent]) -> Vec<String> {
        let mut lines = Vec::new();
        for event in events {
            match event {
                BattleEvent::StatusInflicted { target, status } => {
                    self.status_counts[0] += 1;
                    lines.push(format!("status inflicted battle={} actor={} status={}", id.0, target.0, status.name()));
                }
                BattleEvent::StatusEnded { target, status } => {
                    self.status_counts[1] += 1;
                    lines.push(format!("status ended battle={} actor={} status={}", id.0, target.0, status.name()));
                }
                BattleEvent::StatusTick { .. } => self.status_counts[2] += 1,
                _ => {}
            }
        }
        lines
    }

    /// Count the enemy attacks among `events`, the events of ONE COMMAND's batch, and say each as a log line:
    /// `counter battle=.. actor=.. attack=..`. Every enemy attack in such a batch is a counter, an enemy script's answer to
    /// the blow: submitting a command never advances the clock (an enemy's own turn only ever comes with a step), so an
    /// enemy that acts inside a command's batch is answering that command. (A Death counter that the CLOCK runs, after a
    /// turn of Poison killed its enemy, comes in a step's events and is not in this count.)
    fn note_counters(&mut self, id: BattleId, events: &[BattleEvent]) -> Vec<String> {
        let mut lines = Vec::new();
        for event in events {
            if let BattleEvent::Action { actor, action: ActionRef::EnemyAttack(attack) } = event {
                self.counters += 1;
                lines.push(format!("counter battle={} actor={} attack={attack}", id.0, actor.0));
            }
        }
        lines
    }

    /// Whole milliseconds since the last step, bounded by what one engine step takes; the clock
    /// moves by exactly what is returned, so no fraction of a millisecond is ever lost.
    fn take_elapsed_ms(&mut self, now: Instant) -> u32 {
        let elapsed = now.saturating_duration_since(self.clock);
        if elapsed.as_millis() > u128::from(MAX_STEP_MS) {
            // A stall far longer than a step: drop the excess instead of owing it forever.
            self.clock = now;
            return MAX_STEP_MS;
        }
        let ms = elapsed.as_millis() as u32;
        self.clock += Duration::from_millis(u64::from(ms));
        ms
    }

    /// Note which gauges are full now and since when (`ready_since`): a gauge that was full stays dated from when it
    /// filled, one that is not full (it is filling, its owner acted or died) loses its date.
    fn note_ready(&mut self) {
        let now = self.state.tick();
        let ready = self.state.ready_actors();
        for (i, since) in self.ready_since.iter_mut().enumerate() {
            let is_ready = u8::try_from(i).is_ok_and(|i| ready.contains(&ActorId(i)));
            *since = match (is_ready, *since) {
                (true, Some(at)) => Some(at),
                (true, None) => Some(now),
                (false, _) => None,
            };
        }
    }

    /// The living, connected players as the companions' pacing sees them (`companion::may_act`). A player that cannot
    /// take a turn (asleep, stopped or paralysed: it holds its full gauge but gives no command) owes none, so it holds
    /// nobody up.
    fn player_turns(&self) -> Vec<companion::PlayerTurn> {
        self.members
            .iter()
            .enumerate()
            .filter(|(i, m)| {
                m.info.is_player() && m.connected && u8::try_from(*i).ok().and_then(|i| self.state.actor(ActorId(i))).is_some_and(Combatant::can_act)
            })
            .map(|(i, _)| companion::PlayerTurn {
                commands: self.commands.get(i).copied().unwrap_or(0),
                ready_since: self.ready_since.get(i).copied().flatten(),
            })
            .collect()
    }

    fn connected(&self) -> Vec<EntityId> {
        self.members.iter().filter(|m| m.connected).filter_map(|m| m.info.entity()).collect()
    }

    fn actor_of(&self, entity: EntityId) -> Option<ActorId> {
        let i = self.members.iter().position(|m| m.info.entity() == Some(entity))?;
        u8::try_from(i).ok().map(ActorId)
    }
}

/// Everything the world keeps for battles. Absent (`None` in `World`) on a server that cannot run
/// them.
pub(super) struct BattleWorld {
    settings: BattleSettings,
    debug_encounters: bool,
    battles: BTreeMap<BattleId, Battle>,
    next_id: u64,
    /// Seeds, random-encounter rolls and weighted formation picks.
    rng: Rng,
}

/// An area's encounters as they run here: its table (only a table that can trigger at all), the
/// distance between two random rolls, and whether random rolls happen. The table is kept even when
/// random encounters are off, so a debug encounter request that names no formation still picks
/// from the area's own table instead of the rules' fallback formation.
#[derive(Debug, Clone)]
pub(super) struct AreaEncounters {
    pub table: EncounterTable,
    /// Horizontal distance between two random-encounter rolls.
    pub step: f64,
    /// Whether walking here rolls random encounters (`--no-random-encounters` turns this off).
    pub random: bool,
}

impl BattleWorld {
    /// The rules over this server's own game data (the characters' experience curves are in them).
    pub(super) fn rules(&self) -> &dyn BattleRules {
        self.settings.rules.as_ref()
    }

    pub(super) fn new(settings: BattleSettings, debug_encounters: bool) -> Self {
        let rng = Rng::new(settings.fixed_seed.unwrap_or_else(fresh_seed));
        Self { settings, debug_encounters, battles: BTreeMap::new(), next_id: 1, rng }
    }

    /// The encounters of `area` under these settings (`None`: it has no usable table). Random rolls
    /// happen only when `random` is set; the table also serves debug requests that name no formation.
    pub(super) fn area_encounters(&self, area: &dyn AreaRuntime) -> Option<AreaEncounters> {
        let table = area.encounters().filter(|t| !t.is_empty())?;
        let step = f64::from(self.settings.encounter_step_units.unwrap_or_else(|| area.max_speed())).max(1.0);
        Some(AreaEncounters { table, step, random: self.settings.random_encounters })
    }

    /// The seed of the next battle.
    fn next_seed(&mut self) -> u64 {
        self.settings.fixed_seed.unwrap_or_else(|| (u64::from(self.rng.next_u32()) << 32) | u64::from(self.rng.next_u32()))
    }

    /// A weighted pick from `table`.
    fn pick_from(&mut self, table: &EncounterTable) -> Option<u16> {
        let total = table.total_weight();
        let roll = self.rng.below(total);
        table.pick(roll)
    }

    /// The formation for a debug request that names none: the operator's choice, else a pick from
    /// the area's table, else the rules' default.
    fn default_formation(&mut self, table: Option<&EncounterTable>) -> Option<u16> {
        self.settings
            .debug_formation
            .or_else(|| table.and_then(|t| self.pick_from(t)))
            .or_else(|| self.settings.rules.default_formation())
    }
}

/// A seed that differs between battles and between runs (not secret: it is sent in the clear).
fn fresh_seed() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    splitmix64(nanos ^ n.wrapping_mul(0x9E37_79B9_7F4A_7C15))
}

fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// A command as one or two `key=value` words of a log line: `attack target=2`, `magic spell=0 target=1`, `magic spell=0
/// target=all-allies`, `limit attack=128 target=2` (`limit_id` is the limit's id when it is known), `defend`.
fn describe_command(command: &Command, limit_id: Option<u16>) -> String {
    match command {
        Command::Attack { target } => format!("attack target={}", target.0),
        Command::Magic { spell, target } => format!("magic spell={spell} target={}", target_word(*target)),
        Command::Item { item, target } => format!("item item={item} target={}", target_word(*target)),
        Command::Defend => "defend".to_string(),
        Command::Limit { target } => {
            format!("limit attack={} target={}", limit_id.map_or_else(|| "?".to_string(), |id| id.to_string()), target_word(*target))
        }
        Command::Escape => "escape".to_string(),
    }
}

/// How a target reads in a log line: the actor's number for one actor (the layout the smoke scripts have always
/// read), `all-enemies` or `all-allies` for a whole side (the sides are the commanding actor's own).
fn target_word(target: Target) -> String {
    match target {
        Target::One(id) => id.0.to_string(),
        Target::AllEnemies => "all-enemies".to_string(),
        Target::AllAllies => "all-allies".to_string(),
    }
}

/// The wire reason for an engine refusal.
fn rejection_of(e: CommandError) -> BattleRejection {
    match e {
        CommandError::BattleOver => BattleRejection::BattleOver,
        CommandError::UnknownActor(_) | CommandError::NotControllable(_) => BattleRejection::NotYourActor,
        CommandError::ActorDead(_) => BattleRejection::ActorDead,
        CommandError::GaugeNotFull(_) => BattleRejection::GaugeNotFull,
        // A way of aiming the action does not allow (one target or a whole side, protocol v10) and a caster with
        // no All use left are both "not a valid target": the menu never offers either, so the one word is enough.
        CommandError::UnknownTarget(_)
        | CommandError::InvalidTarget(_)
        | CommandError::InvalidReach(_)
        | CommandError::NoAllLeft(_) => BattleRejection::InvalidTarget,
        CommandError::TargetDead(_) => BattleRejection::TargetDead,
        // A revival of a whole side with nobody fallen: the action only revives the dead.
        CommandError::TargetNotDead(_) | CommandError::NoTargets(_) => BattleRejection::TargetNotDead,
        CommandError::UnknownSpell(_) => BattleRejection::UnknownSpell,
        CommandError::NotEnoughMp(_) => BattleRejection::NotEnoughMp,
        CommandError::UnknownItem(_) => BattleRejection::UnknownItem,
        CommandError::NoLimit(_) => BattleRejection::NoLimit,
        CommandError::LimitNotReady(_) => BattleRejection::LimitNotReady,
        CommandError::CannotEscape => BattleRejection::CannotEscape,
        // A sleeper, a stopped or a paralysed member holds its turn; a silenced one cannot cast (statuses).
        CommandError::CannotAct(_) => BattleRejection::CannotAct,
        CommandError::Silenced(_) => BattleRejection::Silenced,
    }
}

impl World {
    /// Switch battles on (`settings` of `Some`) or leave them unsupported (`None`, every request is
    /// refused). The encounter table of every loaded area is read here, once; an area loaded later
    /// reads its own when it is loaded.
    pub fn configure_battle(&mut self, settings: Option<BattleSettings>, debug_encounters: bool) {
        self.battle = settings.map(|s| BattleWorld::new(s, debug_encounters));
        for area in self.areas.values_mut() {
            area.refresh_encounters(self.battle.as_ref());
        }
    }

    /// Whether this world can run battles at all.
    #[cfg(test)]
    pub fn battles_supported(&self) -> bool {
        self.battle.is_some()
    }

    /// How many battles are running.
    #[cfg(test)]
    pub fn battle_count(&self) -> usize {
        self.battle.as_ref().map_or(0, |b| b.battles.len())
    }

    /// Is `entity` in a battle (frozen)?
    #[cfg(test)]
    pub fn in_battle(&self, entity: EntityId) -> bool {
        self.players.get(&entity).is_some_and(|p| p.battle.is_some())
    }

    /// `ClientMsg::RequestEncounter`: start a battle for `entity`'s party, if the server allows
    /// debug encounters. `formation` of `None` lets the server choose.
    pub fn request_encounter(&mut self, entity: EntityId, formation: Option<u16>, now: Instant) -> BattleReply {
        let Some(player) = self.players.get(&entity) else {
            return BattleReply::Gone;
        };
        let Some(bw) = self.battle.as_mut() else {
            return refused(None, BattleRejection::Unsupported);
        };
        if !bw.debug_encounters {
            return refused(None, BattleRejection::DebugDisabled);
        }
        if player.transit.is_some() {
            // Between two areas there is no field to fight in (the protocol has no sharper word).
            return refused(None, BattleRejection::Unsupported);
        }
        let table = self.areas.get(&player.area).and_then(|a| a.encounters()).map(|e| &e.table);
        let Some(formation) = formation.or_else(|| bw.default_formation(table)) else {
            return refused(None, BattleRejection::UnknownFormation);
        };
        match self.begin_battle(entity, formation, now, None) {
            Ok(_) => BattleReply::Done,
            Err(reason) => refused(None, reason),
        }
    }

    /// A story point of `entity`'s area starts the battle `formation` (`AreaRuntime::story_battle`): the party fights
    /// it now and the point's step is applied if it is won. Returns whether the battle started; when it cannot (the
    /// server runs no battles, a member is already in one) the story waits, and the server says so.
    pub(super) fn start_story_battle(&mut self, entity: EntityId, point: u32, formation: u16, now: Instant) -> bool {
        let Some(area) = self.players.get(&entity).map(|p| p.area.clone()) else { return false };
        let game_area = self.areas.get(&area).map_or_else(|| area.clone(), |a| a.id().to_string());
        match self.begin_battle(entity, formation, now, Some(StoryThen { entity, area, point })) {
            Ok(id) => {
                self.logs.info(format!("story battle started battle={} entity={} area={game_area} point={point} formation={formation}", id.0, entity.0));
                true
            }
            Err(reason) => {
                self.logs.info(format!(
                    "a story battle could not start, the story waits entity={} area={game_area} point={point} formation={formation} reason={reason:?}",
                    entity.0
                ));
                false
            }
        }
    }

    /// Start a battle with `formation` for `requester`'s party (or just `requester`). `then` is the story step the
    /// battle is the price of (applied when it is won), for the battle a story point starts.
    fn begin_battle(&mut self, requester: EntityId, formation: u16, now: Instant, then: Option<StoryThen>) -> Result<BattleId, BattleRejection> {
        let Some(bw) = self.battle.as_mut() else {
            return Err(BattleRejection::Unsupported);
        };
        let Some(player) = self.players.get(&requester) else {
            return Err(BattleRejection::NotInBattle);
        };
        if player.battle.is_some() {
            return Err(BattleRejection::AlreadyInBattle);
        }
        if player.transit.is_some() {
            return Err(BattleRejection::Unsupported);
        }
        let info = bw.settings.rules.formation(formation).ok_or(BattleRejection::UnknownFormation)?;

        // The PLAYERS who fight: the requester and the rest of its party that are here (in its area, not between two
        // areas) and free, in the party's own order (so that the roster agrees with the party panel); the requester
        // alone when it has no party.
        let party_of = self.parties.party_of(player.account);
        let mut entities: Vec<EntityId> = party_of
            .into_iter()
            .flat_map(|party| party.members.iter())
            .filter_map(|member| self.by_account.get(member).copied())
            .filter(|&entity| {
                entity == requester
                    || self.players.get(&entity).is_some_and(|p| p.battle.is_none() && p.transit.is_none() && p.area == player.area)
            })
            .collect();
        if !entities.contains(&requester) {
            entities.insert(0, requester);
        }
        entities.truncate(MAX_BATTLE_PARTY);

        // The CANON party fights beside them (protocol v9): the story's own members, commanded by the server, FIRST in
        // the roster; the players' characters are added after them and nobody is put in a canon member's place. The story
        // is the party LEADER's (every member of a party is in the leader's instance) and so is the level the companions
        // fight at; the leader may be in another area or in a battle of its own and still decides, as long as it is
        // online (else the requester does). `--battle-level` pins the level of the whole fight. Companions are never
        // paid or saved, and start with an empty limit gauge (a full one under `--debug-limit-full`).
        let leader_entity = party_of
            .and_then(|party| self.by_account.get(&party.leader).copied())
            .filter(|entity| self.players.contains_key(entity))
            .unwrap_or(requester);
        let leader = self.players.get(&leader_entity).unwrap_or(player);
        let max_limit = bw.rules().progress_limits().max_limit;
        let companion_level = bw.settings.level.unwrap_or_else(|| leader.progress.unwrap_or_else(|| progress::fresh(bw.rules(), progress::DEFAULT_CHARACTER)).level);
        let companion_limit = if bw.settings.debug_limit_full { max_limit } else { 0 };
        let canon: Vec<u8> = self.areas.get(&player.area).map_or_else(Vec::new, |area| area.runtime().story_party(&leader.progression));
        let companions: Vec<BattleMember> = canon
            .iter()
            .filter_map(|&character| {
                let name = clip_character_name(&bw.rules().character_name(character)?);
                Some(BattleMember { status: bw.settings.debug_status, ..BattleMember::companion(name, character, companion_level, companion_limit) })
            })
            .take(MAX_BATTLE_PARTY.saturating_sub(entities.len()))
            .collect();

        // Each player fights as its own character (its stat curve) at its own level (see `progress`); `--battle-level`
        // pins the level of the fight, not what the account has earned.
        let players: Vec<(EntityId, BattleMember)> = entities
            .iter()
            .filter_map(|&entity| {
                let p = self.players.get(&entity)?;
                let own = p.progress.unwrap_or_else(|| progress::fresh(bw.rules(), progress::DEFAULT_CHARACTER));
                let level = bw.settings.level.unwrap_or(own.level);
                // The limit gauge is the account's own, inside the game's cap (the engine would refuse more);
                // `--debug-limit-full` starts everybody at the cap and `keep_limits` then saves no gauge (module docs). A
                // player has no limit break in v9, so the engine ignores the value: it travels, and it is never rewritten.
                let limit = if bw.settings.debug_limit_full { max_limit } else { own.limit.min(max_limit) };
                // The roster carries the PLAYER'S OWN name (v8): every peer's panel, list and banners show it, while
                // `character` (the stat curve) alone decides which combat template the engine uses.
                let name = clip_character_name(&p.snapshot.avatar.display_name);
                let member = BattleMember { status: bw.settings.debug_status, ..BattleMember::player(entity, p.account, name, own.character, level, limit) };
                Some((entity, member))
            })
            .collect();
        let entities: Vec<EntityId> = players.iter().map(|(entity, _)| *entity).collect();
        let party: Vec<BattleMember> = companions.iter().cloned().chain(players.into_iter().map(|(_, member)| member)).collect();

        let seed = bw.next_seed();
        let setup = BattleSetup {
            formation_id: formation,
            seed,
            party: party.clone(),
            preemptive: info.preemptive,
            back_attack: info.back_attack,
            battle_speed: bw.settings.battle_speed,
            ticks_per_second: bw.settings.ticks_per_second,
        };
        let state = match bw.settings.rules.build(&setup) {
            Ok(state) => state,
            Err(e) => {
                self.logs.warn(format!("cannot build the battle from the server's own data formation={formation} error={e}"));
                return Err(BattleRejection::UnknownFormation);
            }
        };
        let id = BattleId(bw.next_id);
        bw.next_id += 1;
        // How hard the fight is to run from (`none`: it cannot be), for the log.
        let escape = state.escape_left().map_or_else(|| "none".to_string(), |left| left.to_string());
        let start = Arc::new(ServerMsg::EncounterStart {
            battle_id: id,
            formation_id: formation,
            seed,
            party: party.clone(),
            preemptive: setup.preemptive,
            back_attack: setup.back_attack,
            battle_speed: setup.battle_speed,
            ticks_per_second: setup.ticks_per_second,
            data_fingerprint: bw.settings.rules.data_fingerprint(),
        });
        bw.battles.insert(
            id,
            Battle {
                state,
                members: party.iter().cloned().map(|info| Member { connected: info.is_player(), info }).collect(),
                commands: vec![0; party.len()],
                ready_since: vec![None; party.len()],
                clock: now,
                formation,
                seed,
                status_counts: [0; 3],
                then,
                counters: 0,
            },
        );
        for entity in &entities {
            if let Some(p) = self.players.get_mut(entity) {
                p.battle = Some(id);
            }
        }
        // The `key=value` layout is read by `scripts/battle-smoke.sh` ("members=2", and with --progress
        // "levels=[6, 6]"): keep it, and add new keys at the end. `members` is the number of PLAYERS (as it always was)
        // and `companions` how many canon companions go first in the lists, which are one entry per fighter in actor
        // order: `names=[Cloud,Barret,Blizz,Red%20XIII]` are the fighters' names, percent-encoded
        // (`character::log_token`) so that a name with a space never splits the line; `characters` are their
        // stat curves (a companion's own character), in the same order.
        self.logs.info(format!(
            "battle started battle={} formation={formation} members={} enemies={} preemptive={} back_attack={} seed={seed} characters={:?} levels={:?} limits={:?} run_difficulty={escape} names=[{}] companions={} statuses=[{}]",
            id.0,
            entities.len(),
            info.enemies,
            info.preemptive,
            info.back_attack,
            party.iter().map(|m| m.character).collect::<Vec<_>>(),
            party.iter().map(|m| m.level).collect::<Vec<_>>(),
            party.iter().map(|m| m.limit).collect::<Vec<_>>(),
            party.iter().map(|m| log_token(&m.name)).collect::<Vec<_>>().join(","),
            companions.len(),
            party.iter().map(|m| status_word(m.status)).collect::<Vec<_>>().join(","),
        ));
        self.fan_to(&entities, start);
        Ok(id)
    }

    /// `ClientMsg::BattleCommand` from `entity`'s session.
    pub fn battle_command(&mut self, entity: EntityId, battle_id: BattleId, actor: ActorId, command: Command, now: Instant) -> BattleReply {
        let Some(player) = self.players.get(&entity) else {
            return BattleReply::Gone;
        };
        if player.battle != Some(battle_id) {
            return refused(Some(battle_id), BattleRejection::NotInBattle);
        }
        // Bring the engine to this moment first: the command lands at the time it arrived, and
        // whatever happened since the last tick is announced before it.
        self.step_one(battle_id, now);
        let Some(bw) = self.battle.as_mut() else {
            return refused(Some(battle_id), BattleRejection::Unsupported);
        };
        let Some(battle) = bw.battles.get_mut(&battle_id) else {
            // The wait ended the battle (the party fell): its members already heard.
            return refused(Some(battle_id), BattleRejection::BattleOver);
        };
        let Some(own) = battle.actor_of(entity) else {
            return refused(Some(battle_id), BattleRejection::NotInBattle);
        };
        if actor != own {
            return refused(Some(battle_id), BattleRejection::NotYourActor);
        }
        // The engine's own checks come first (gauge, target, MP, spell and item known): what it would refuse is
        // refused the same way whether or not the account owns the item, and the check changes nothing.
        if let Err(e) = battle.state.check_command(actor, &command) {
            return refused(Some(battle_id), rejection_of(e));
        }
        // The engine knows no inventory: an item needs one in the ACCOUNT's, checked here.
        if let Command::Item { item, .. } = command {
            let owns = self.players.get(&entity).and_then(|p| p.inventory.as_ref()).is_some_and(|inventory| inventory.count(item) > 0);
            if !owns {
                return refused(Some(battle_id), BattleRejection::OutOfItems);
            }
        }
        let who = battle.members.get(usize::from(actor.0)).and_then(|m| Some((m.info.account()?, m.info.character)));
        // The player's own name, for the log lines below (`name=`, last: the lines are read by key).
        let name = battle.members.get(usize::from(actor.0)).map(|m| log_token(&m.info.name)).unwrap_or_default();
        // A limit says which one it is, before it empties the gauge (for the log).
        let limit_id = battle.state.limit_for(actor).map(|l| l.attack);
        match battle.state.submit_command(actor, command) {
            Ok(events) => {
                if let Some(n) = battle.commands.get_mut(usize::from(actor.0)) {
                    *n += 1;
                }
                // Magic, defends, limits and escapes are logged as they happen (`key=value`, new keys at the
                // end); the number a failed escape has left is what the next attempt starts from. (An item is
                // logged by `spend_item`, after the engine took it; a plain attack is not logged.)
                let note = match (command, who) {
                    (Command::Magic { spell, target }, Some((account, character))) => Some(format!(
                        "magic used battle={} actor={} account={} character={character} spell={spell} target={} name={name}",
                        battle_id.0,
                        actor.0,
                        account.0,
                        target_word(target)
                    )),
                    (Command::Defend, Some((account, character))) => Some(format!(
                        "defend used battle={} actor={} account={} character={character} name={name}",
                        battle_id.0, actor.0, account.0
                    )),
                    (Command::Limit { target }, Some((account, character))) => Some(format!(
                        "limit used battle={} actor={} account={} character={character} attack={} target={} name={name}",
                        battle_id.0,
                        actor.0,
                        account.0,
                        limit_id.map_or_else(|| "?".to_string(), |id| id.to_string()),
                        target_word(target)
                    )),
                    (Command::Escape, Some((account, character))) => Some(format!(
                        "escape tried battle={} actor={} account={} character={character} result={} difficulty_left={} name={name}",
                        battle_id.0,
                        actor.0,
                        account.0,
                        if battle.state.outcome() == Some(Outcome::Escaped) { "escaped" } else { "failed" },
                        battle.state.escape_left().unwrap_or(0)
                    )),
                    _ => None,
                };
                // What the engine announced about statuses, and the counters that answered this command.
                let mut event_notes = battle.note_statuses(battle_id, &events);
                event_notes.extend(battle.note_counters(battle_id, &events));
                let msg = ServerMsg::BattleEvent {
                    battle_id,
                    tick: battle.state.tick(),
                    events,
                    command: Some(AppliedCommand { actor, command }),
                };
                let (ended, recipients) = (battle.state.outcome().is_some(), battle.connected());
                if let Some(note) = note {
                    self.logs.info(note);
                }
                for line in event_notes {
                    self.logs.info(line);
                }
                self.fan_to(&recipients, Arc::new(msg));
                // The engine took the command, so the item is spent (even if the player dropped meanwhile).
                if let (Command::Item { item, .. }, Some((account, character))) = (command, who) {
                    self.spend_item(battle_id, entity, account, character, item);
                }
                if ended {
                    self.finish_battle(battle_id);
                }
                BattleReply::Done
            }
            Err(e) => refused(Some(battle_id), rejection_of(e)),
        }
    }

    /// An item of `account` was used in battle `battle`: the player's mirror loses one (if it is still
    /// online), the saver is queued the change (whether or not it is) and the player is told what it carries
    /// now. The player was `entity` when it gave the command.
    fn spend_item(&mut self, battle: BattleId, entity: EntityId, account: AccountId, character: u8, item: u16) {
        let mut told: Option<Outbound> = None;
        let mut left = None;
        if let Some(inventory) = self.players.get_mut(&entity).and_then(|p| p.inventory.as_mut()) {
            inventory.take_one(item);
            left = Some(inventory.count(item));
            told = Some(Arc::new(inventory.message()));
        }
        let saving = match self.saver.as_ref().map(|saver| saver.save(SaveJob::used_item(account, character, item))) {
            Some(true) => "queued",
            Some(false) => "NOT QUEUED (the saver has stopped: the server is shutting down)",
            None => "off (no saver)",
        };
        self.logs.info(format!(
            "item used battle={} account={} character={character} item={item} left={} saving={saving}",
            battle.0,
            account.0,
            left.map_or_else(|| "?".to_string(), |n| n.to_string()),
        ));
        if let Some(msg) = told {
            self.fan_to(&[entity], msg);
        }
    }

    /// Advance every battle to `now` and announce what happened. Call it every tick.
    pub fn advance_battles(&mut self, now: Instant) {
        let ids: Vec<BattleId> = match &self.battle {
            Some(bw) => bw.battles.keys().copied().collect(),
            None => return,
        };
        for id in ids {
            self.step_one(id, now);
        }
    }

    /// Advance one battle's engine to `now`, tell its members what that produced (only if it
    /// produced something) and end it if it ended.
    fn step_one(&mut self, id: BattleId, now: Instant) {
        let Some(bw) = self.battle.as_mut() else { return };
        let Some(battle) = bw.battles.get_mut(&id) else { return };
        let ms = battle.take_elapsed_ms(now);
        let events = battle.state.step(ms);
        let status_notes = battle.note_statuses(id, &events);
        let (tick, ended, recipients) = (battle.state.tick(), battle.state.outcome().is_some(), battle.connected());
        if !events.is_empty() {
            self.fan_to(&recipients, Arc::new(ServerMsg::BattleEvent { battle_id: id, tick, events, command: None }));
        }
        for line in status_notes {
            self.logs.info(line);
        }
        if ended {
            self.finish_battle(id);
        } else {
            // Whatever the step made ready, the canon companions answer at once (module docs, "Companions").
            self.companions_act(id);
        }
    }

    /// The canon companions whose ATB gauge is full give their commands now, one at a time in actor order, each chosen by
    /// [`companion::command`] from the engine's own state and applied like a player's: the engine checks and submits it,
    /// and every member is told with a `BattleEvent` that carries the command, so every client replays it with the
    /// engine code it already has. The command is counted for the actor, and logged as
    /// `companion acted battle=.. actor=.. name=.. command=.. character=..` (and nowhere else: a companion's magic or limit
    /// is not a `magic used` or `limit used` line, which are the players'). A command that ends the battle ends it here.
    /// Nothing about a companion is ever paid or saved.
    fn companions_act(&mut self, id: BattleId) {
        // A companion acts once per full gauge and the gauge empties, so a round cannot take more turns than the party has
        // members; the bound is only a guard.
        for _ in 0..MAX_BATTLE_PARTY {
            let acted = {
                let Some(bw) = self.battle.as_mut() else { return };
                let pacing = bw.settings.companion_pacing;
                let Some(battle) = bw.battles.get_mut(&id) else { return };
                if battle.state.outcome().is_some() {
                    return;
                }
                // The players go first (`companion::may_act`): a companion whose gauge is full holds it while a living,
                // connected player still owes a turn, within the pacing's bounds.
                battle.note_ready();
                let (now, tps, players) = (battle.state.tick(), battle.state.config().ticks_per_second, battle.player_turns());
                let next = battle.state.ready_actors().into_iter().find_map(|actor| {
                    let index = usize::from(actor.0);
                    let member = battle.members.get(index)?;
                    member.info.is_companion().then_some(())?;
                    let since = battle.ready_since.get(index).copied().flatten().unwrap_or(now);
                    let own = battle.commands.get(index).copied().unwrap_or(0);
                    companion::may_act(pacing, tps, now, own, since, &players).then_some(())?;
                    companion::command(&battle.state, actor).map(|command| (actor, command, now.saturating_sub(since)))
                });
                let Some((actor, command, held_ticks)) = next else { return };
                let held_ms = held_ticks * 1000 / u64::from(tps.max(1));
                let index = usize::from(actor.0);
                let (name, character) = battle.members.get(index).map(|m| (log_token(&m.info.name), m.info.character)).unwrap_or_default();
                // A limit says which one it is, before it empties the gauge (for the log).
                let limit_id = battle.state.limit_for(actor).map(|l| l.attack);
                match battle.state.submit_command(actor, command) {
                    Ok(events) => {
                        if let Some(n) = battle.commands.get_mut(index) {
                            *n += 1;
                        }
                        let note = format!(
                            "companion acted battle={} actor={} name={name} command={} character={character} held_ms={held_ms}",
                            id.0,
                            actor.0,
                            describe_command(&command, limit_id)
                        );
                        let mut event_notes = battle.note_statuses(id, &events);
                        event_notes.extend(battle.note_counters(id, &events));
                        let msg = ServerMsg::BattleEvent {
                            battle_id: id,
                            tick: battle.state.tick(),
                            events,
                            command: Some(AppliedCommand { actor, command }),
                        };
                        Ok((note, event_notes, msg, battle.state.outcome().is_some(), battle.connected()))
                    }
                    // Not reachable (the rule only picks what `check_command` accepts, in the same state): said loudly
                    // and the round ends rather than trying the same thing again.
                    Err(e) => Err(format!("a companion's command was refused by the engine battle={} actor={} name={name} command={command:?} error={e}", id.0, actor.0)),
                }
            };
            match acted {
                Ok((note, event_notes, msg, ended, recipients)) => {
                    self.logs.info(note);
                    for line in event_notes {
                        self.logs.info(line);
                    }
                    self.fan_to(&recipients, Arc::new(msg));
                    if ended {
                        self.finish_battle(id);
                        return;
                    }
                }
                Err(why) => {
                    self.logs.warn(why);
                    return;
                }
            }
        }
    }

    /// End a battle whose engine has an outcome: tell the members, free them, log the rewards.
    fn finish_battle(&mut self, id: BattleId) {
        let Some(bw) = self.battle.as_mut() else { return };
        let Some(outcome) = bw.battles.get(&id).and_then(|b| b.state.outcome()) else { return };
        let rules = Arc::clone(&bw.settings.rules);
        let Some(battle) = bw.battles.remove(&id) else { return };
        // The same players-only roster every peer's replay splits the rewards over (`reward_roster`).
        let roster = reward_roster(battle.members.iter().map(|m| &m.info), &battle.state);
        let mut rewards = if outcome == Outcome::Victory { split_rewards(battle.state.rewards(), &roster) } else { Vec::new() };
        if outcome == Outcome::Victory {
            // The drops are rolled now, after the outcome, from the battle's seed on a stream of its own: they
            // are not part of the replayed battle (a client cannot check them), and they are logged.
            let drops = rules.roll_drops(battle.formation, battle.seed);
            assign_drops(&mut rewards, &roster, &drops, rules.max_item_stack());
            self.logs.info(format!(
                "battle drops battle={} formation={} seed={} items={:?}",
                id.0,
                battle.formation,
                battle.seed,
                drops.iter().map(|d| (d.item, d.count)).collect::<Vec<_>>(),
            ));
        }
        // The `key=value` layout is read by `scripts/battle-smoke.sh` ("commands=[2, 3]"): keep it.
        // `members` counts the PLAYERS (as it always did); `commands` and `limits` have one entry per fighter in actor
        // order, the `companions` canon companions first.
        self.logs.info(format!(
            "battle ended battle={} outcome={outcome:?} ticks={} members={} commands={:?} limits={:?} companions={} status_inflicted={} status_ended={} status_ticks={} counters={}",
            id.0,
            battle.state.tick(),
            battle.members.iter().filter(|m| m.info.is_player()).count(),
            battle.commands,
            (0..battle.members.len()).map(|i| u8::try_from(i).map_or(0, |i| battle.state.limit_gauge(ActorId(i)))).collect::<Vec<_>>(),
            battle.members.iter().filter(|m| m.info.is_companion()).count(),
            battle.status_counts[0],
            battle.status_counts[1],
            battle.status_counts[2],
            battle.counters,
        ));
        let told = self.pay_members(id, &battle.members, &rewards);
        self.keep_limits(id, &battle);
        let recipients = battle.connected();
        // Back in the field: free everyone first, and mark them changed so the next `EntityUpdate`
        // says where they stand.
        for entity in battle.members.iter().filter_map(|m| m.info.entity()) {
            if let Some(p) = self.players.get_mut(&entity) {
                if p.battle == Some(id) {
                    p.battle = None;
                    p.dirty = true;
                    p.walked = 0.0;
                }
            }
        }
        self.fan_to(&recipients, Arc::new(ServerMsg::BattleEnd { battle_id: id, outcome, rewards }));
        // Where each paid player's character stands now, right after the battle's end.
        for (entity, msg) in told {
            self.fan_to(&[entity], msg);
        }
        // A battle a story point started is the price of the point's step: paid when it is won, never when it is lost
        // or fled (the story stays where it was, and the point can be reached again).
        if outcome == Outcome::Victory {
            if let Some(then) = battle.then.clone() {
                self.story_after_battle(then);
            }
        }
    }

    /// Keep the limit gauge every member of a finished battle ended with, whatever the outcome (an
    /// escape pays nothing, but its gauges are kept), EXCEPT under `--debug-limit-full`, where every
    /// starting gauge was faked and nothing is kept (logged as `limit not kept ... reason=debug-limit-full`).
    /// Otherwise a member whose gauge is not the one it started with
    /// has the new one queued for the saver (whether or not it is still connected: its character fought
    /// on) and put in its mirror (found by its account, which may have logged in again). A gauge that did
    /// not move writes nothing. A knocked-out character ended with an empty one, which is what is kept.
    fn keep_limits(&mut self, id: BattleId, battle: &Battle) {
        // `--debug-limit-full` faked every starting gauge: what a member ends with is not its account's
        // own gauge, so nothing is stored and a test run can never overwrite a real account's gauge.
        if self.battle.as_ref().is_some_and(|bw| bw.settings.debug_limit_full) {
            self.logs.info(format!("limit not kept battle={} reason=debug-limit-full", id.0));
            return;
        }
        // `--debug-bonus-limit`: the gauge kept is the one the battle left plus the bonus, up to the game's cap. It goes
        // down the same road as any other (the saver, the mirror, the `limit kept` line, which says so).
        let bonus = self.battle.as_ref().map_or(0, |bw| u16::from(bw.settings.debug_bonus_limit));
        let cap = self.battle.as_ref().map_or(u16::MAX, |bw| bw.rules().progress_limits().max_limit);
        for (i, member) in battle.members.iter().enumerate() {
            let Some(actor) = u8::try_from(i).ok().map(ActorId) else { continue };
            // Only a member the engine gave a limit break has a gauge of its own to keep: a player has none (protocol
            // v9), so the gauge its account holds is left exactly as it is and is never overwritten by the 0 the
            // engine reports for a character without one (and the debug bonus has no gauge to add to either).
            let Some(account) = member.info.account().filter(|_| battle.state.limit_for(actor).is_some()) else { continue };
            let end = battle.state.limit_gauge(actor);
            let end = if bonus > 0 { end.saturating_add(bonus).min(cap) } else { end };
            if end == member.info.limit {
                continue;
            }
            let character = member.info.character;
            let queued = self.saver.as_ref().map(|saver| saver.save(SaveJob::limit(account, character, end)));
            let online = self.by_account.get(&account).copied();
            let mirrored = online.and_then(|entity| self.players.get_mut(&entity)).and_then(|p| p.progress.as_mut()).map(|own| own.limit = end).is_some();
            let saving = match queued {
                Some(true) => "queued",
                Some(false) => "NOT QUEUED (the saver has stopped: the server is shutting down)",
                None => "off (no saver)",
            };
            let debug = if bonus > 0 { format!(" debug_bonus_limit={bonus}") } else { String::new() };
            self.logs.info(format!(
                "limit kept battle={} account={} character={character} limit={end} was={} mirrored={mirrored} saving={saving}{debug}",
                id.0, account.0, member.info.limit
            ));
        }
    }

    /// Pay what a won battle earned (`rewards`, one entry per member of the roster) into each
    /// member's character progress (see the module docs). Every payment is queued for the saver,
    /// whether or not the member is still connected; the member that is online (found by its account,
    /// not by the entity it fought with: it may have logged in again) has its mirror updated, and the
    /// message that tells it is returned, to go out after `BattleEnd`. A payment of nothing (a member
    /// that was dead at the end) changes nothing: it is not queued and the mirror stays as it is, but
    /// the member that is online is still told where it stands (`leveled_up: false`), so that after a
    /// win every online member of the roster gets `BattleEnd` and then `CharacterProgress`.
    fn pay_members(&mut self, id: BattleId, members: &[Member], rewards: &[BattleReward]) -> Vec<(EntityId, Outbound)> {
        let mut told = Vec::new();
        let Some(rules) = self.battle.as_ref().map(|bw| Arc::clone(&bw.settings.rules)) else { return told };
        let bonus_exp = self.battle.as_ref().map_or(0, |bw| bw.settings.debug_bonus_exp);
        for r in rewards {
            let Some(member) = members.iter().find(|m| m.info.account() == Some(r.account)) else { continue };
            let connected = member.connected;
            if r.is_nothing() {
                let online = self.by_account.get(&r.account).copied();
                if let Some((entity, own)) = online.and_then(|entity| self.players.get(&entity).and_then(|p| p.progress.map(|own| (entity, own)))) {
                    told.push((entity, Arc::new(progress::message(rules.as_ref(), &own, false))));
                }
                self.logs.info(format!(
                    "battle reward battle={} account={} exp=0 gil=0 ap=0 delivered={connected}: nothing to pay (no reward for this member: dead at the end, or the formation gives none), nothing saved",
                    id.0, r.account.0
                ));
                continue;
            }
            // `--debug-bonus-exp`: a member that fought on is paid the bonus on top of what the battle gave it. This is what
            // is paid, saved, applied to the mirror and logged; the `BattleEnd` the clients verify by replay was built
            // before and says what the battle itself earned.
            let boosted;
            let r = if bonus_exp > 0 {
                boosted = BattleReward { exp: r.exp.saturating_add(bonus_exp), ..r.clone() };
                &boosted
            } else {
                r
            };
            // The store is first in line: it gets every payment, a player online or not.
            let queued = self.saver.as_ref().map(|saver| saver.save(SaveJob::reward(r.account, member.info.character, r.clone())));
            let mut effect = String::from("no mirror: the player is not online");
            let online = self.by_account.get(&r.account).copied();
            if let Some((entity, player)) = online.and_then(|entity| self.players.get_mut(&entity).map(|p| (entity, p))) {
                if let Some(own) = player.progress.as_mut() {
                    let before = *own;
                    let leveled_up = progress::apply_reward(rules.as_ref(), own, r);
                    effect = format!(
                        "level {}->{} exp={} gil={} ap={} leveled_up={leveled_up}",
                        before.level, own.level, own.exp, own.gil, own.ap
                    );
                    told.push((entity, Arc::new(progress::message(rules.as_ref(), own, leveled_up))));
                }
                // The drops go into the mirror like the numbers do, and the player is told what it carries.
                if !r.items.is_empty() {
                    if let Some(inventory) = player.inventory.as_mut() {
                        inventory.apply(&Change::Found(r.items.clone()), rules.max_item_stack());
                        effect.push_str(&format!(" items={}", inventory.total()));
                        told.push((entity, Arc::new(inventory.message())));
                    }
                }
            }
            let saving = match queued {
                Some(true) => "queued",
                Some(false) => "NOT QUEUED (the saver has stopped: the server is shutting down)",
                None => "off (no saver)",
            };
            // New keys go at the end of the `key=value` part (scripts read it by key).
            let debug = if bonus_exp > 0 { format!(" debug_bonus_exp={bonus_exp}") } else { String::new() };
            self.logs.info(format!(
                "battle reward battle={} account={} exp={} gil={} ap={} delivered={connected} saving={saving} dropped={:?}{debug}: {effect}",
                id.0,
                r.account.0,
                r.exp,
                r.gil,
                r.ap,
                r.items.iter().map(|e| (e.item, e.count)).collect::<Vec<_>>(),
            ));
        }
        told
    }

    /// Tell `entity` where its character stands (`ServerMsg::CharacterProgress`); a player without
    /// character progress (a server that runs no battles) is told nothing. Players whose queue fails
    /// are appended to `failed` for the caller to drop.
    pub(super) fn tell_progress(&mut self, entity: EntityId, leveled_up: bool, failed: &mut Vec<(EntityId, bool)>) {
        let Some(bw) = self.battle.as_ref() else { return };
        let Some(player) = self.players.get(&entity) else { return };
        let Some(own) = player.progress.as_ref() else { return };
        let msg: Outbound = Arc::new(progress::message(bw.rules(), own, leveled_up));
        if let Err(e) = player.tx.try_send(msg) {
            failed.push((entity, is_full(&e)));
        }
    }

    /// Tell `entity` what it carries (`ServerMsg::Inventory`); a player without an inventory (a server that
    /// runs no battles) is told nothing. Players whose queue fails are appended to `failed` for the caller
    /// to drop.
    pub(super) fn tell_inventory(&mut self, entity: EntityId, failed: &mut Vec<(EntityId, bool)>) {
        let Some(player) = self.players.get(&entity) else { return };
        let Some(inventory) = player.inventory.as_ref() else { return };
        let msg: Outbound = Arc::new(inventory.message());
        if let Err(e) = player.tx.try_send(msg) {
            failed.push((entity, is_full(&e)));
        }
    }

    /// A player that was in battle `id` left the world: its character stays in the engine,
    /// uncontrolled, and the battle is dropped when no player is left to see it (a companion is never connected, so
    /// "every member is gone" means every player).
    pub(super) fn member_gone(&mut self, id: BattleId, entity: EntityId) {
        let Some(bw) = self.battle.as_mut() else { return };
        let Some(battle) = bw.battles.get_mut(&id) else { return };
        for m in &mut battle.members {
            if m.info.entity() == Some(entity) {
                m.connected = false;
            }
        }
        if battle.members.iter().all(|m| !m.connected) {
            bw.battles.remove(&id);
            self.logs.info(format!("battle dropped: every player left battle={}", id.0));
        }
    }

    /// Count `dist` units of walking towards a random-encounter roll and start a battle when one
    /// triggers (see the module docs). Does nothing without battle support or an encounter table.
    pub(super) fn roll_encounter(&mut self, entity: EntityId, dist: f64, now: Instant) {
        let triggered = {
            let Some(bw) = self.battle.as_mut() else { return };
            let Some(p) = self.players.get_mut(&entity) else { return };
            // The table of the area the player walks in.
            let Some(encounters) = self.areas.get(&p.area).and_then(|a| a.encounters()).filter(|e| e.random) else { return };
            let rate = u32::from(encounters.table.rate);
            let step = encounters.step;
            p.walked += dist;
            let rolls = ((p.walked / step) as u32).min(MAX_ROLLS_PER_MOVE);
            p.walked = (p.walked - f64::from(rolls) * step).clamp(0.0, step);
            let mut hit = false;
            for _ in 0..rolls {
                if bw.rng.below(1024) < rate {
                    hit = true;
                    break;
                }
            }
            if hit {
                bw.pick_from(&encounters.table)
            } else {
                None
            }
        };
        if let Some(formation) = triggered {
            if let Err(reason) = self.begin_battle(entity, formation, now, None) {
                self.logs.debug(format!("a random encounter could not start entity={} formation={formation} reason={reason:?}", entity.0));
            }
        }
    }

    /// Queue `msg` for each of `entities` that is in the world; players whose queue is full or
    /// closed are dropped (through the one removal path).
    fn fan_to(&mut self, entities: &[EntityId], msg: Outbound) {
        let mut failed = Vec::new();
        for entity in entities {
            if let Some(p) = self.players.get(entity) {
                if let Err(e) = p.tx.try_send(Arc::clone(&msg)) {
                    failed.push((*entity, is_full(&e)));
                }
            }
        }
        self.drop_failed(failed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use crate::world::{MoveOutcome, NewPlayer};
    use fmv_adapter_api::{AreaRuntime, EncounterEntry};
    use fmv_core::{AccountId, AnimState, AvatarAppearance, Pos};
    use fmv_fixture::{FixtureAdapter, FIXTURE_AREA_KEY};
    use fmv_adapter_api::{AreaInfo, Bounds, GameAdapter, MoveResult};
    use crate::progress::{Progress, ProgressSeed};
    use crate::saver::{Msg, SaverHandle};
    use fmv_protocol::battle::BattleEvent;
    use fmv_protocol::{BattleReplay, InventoryEntry, SyntheticRules};
    use tokio::sync::{mpsc, watch};

    struct Seat {
        entity: EntityId,
        account: AccountId,
        rx: mpsc::Receiver<Outbound>,
        /// Every battle message this seat received, in order.
        log: Vec<ServerMsg>,
    }

    impl Seat {
        fn drain(&mut self) -> Vec<ServerMsg> {
            let mut out = Vec::new();
            while let Ok(m) = self.rx.try_recv() {
                out.push((*m).clone());
            }
            self.log.extend(out.iter().cloned());
            out
        }

        fn battle_msgs(&mut self) -> Vec<ServerMsg> {
            self.drain();
            self.log
                .iter()
                .filter(|m| {
                    matches!(
                        m,
                        ServerMsg::EncounterStart { .. }
                            | ServerMsg::BattleEvent { .. }
                            | ServerMsg::BattleEnd { .. }
                            | ServerMsg::BattleRejected { .. }
                    )
                })
                .cloned()
                .collect()
        }
    }

    fn settings(rules: SyntheticRules) -> BattleSettings {
        BattleSettings {
            battle_speed: 0,
            ticks_per_second: 1000,
            fixed_seed: Some(7),
            ..BattleSettings::new(Arc::new(rules))
        }
    }

    /// `settings` with the companions' pacing off: they act the instant their gauge fills. For the tests about WHAT a
    /// companion does and how its command travels; WHEN it acts (the players first) has tests of its own.
    fn settings_unpaced(rules: SyntheticRules) -> BattleSettings {
        BattleSettings { companion_pacing: companion::Pacing::OFF, ..settings(rules) }
    }

    /// The synthetic rules with a limit break for the PLAYERS' fighters too: the real rules give them none (protocol v9,
    /// `fmv_protocol::PLAYER_LIMIT_BREAKS`), and these tests are about what the server does with a limit gauge, a
    /// command and an end-of-battle save, which no real player reaches while that stays so.
    fn player_limit_rules() -> SyntheticRules {
        SyntheticRules { player_limits: true, ..SyntheticRules::default() }
    }

    fn world_with(settings: Option<BattleSettings>, debug: bool) -> World {
        let mut w = World::new(FixtureAdapter::new().load_area(FIXTURE_AREA_KEY).unwrap()).unwrap();
        w.configure_battle(settings, debug);
        w
    }

    fn seat(w: &mut World, name: &str, now: Instant) -> Seat {
        seat_as(w, name, now, ProgressSeed::default())
    }

    /// A player whose login found `progress` (who its look names and what the store held).
    fn seat_as(w: &mut World, name: &str, now: Instant, progress: ProgressSeed) -> Seat {
        let (session, entity) = w.allocate_ids();
        let (tx, rx) = mpsc::channel(4096);
        let close = Arc::new(watch::channel(false).0);
        let avatar = AvatarAppearance { display_name: name.into(), tint: [1, 2, 3], appearance_ref: None };
        let account = AccountId(entity.0);
        w.join(NewPlayer { session, entity, account, avatar, progression: Vec::new(), progress, tx, close }, now).expect("joins");
        Seat { entity, account, rx, log: Vec::new() }
    }

    /// Two players in one party, the second one playing `character` (the first plays Cloud).
    fn party_of_two_as(w: &mut World, now: Instant, a: ProgressSeed, b: ProgressSeed) -> (Seat, Seat) {
        let (a, b) = (seat_as(w, "A", now, a), seat_as(w, "B", now, b));
        assert_eq!(w.party_invite(a.entity, b.entity, now), crate::world::PartyOutcome::Done);
        let invite = w.parties.invites().next().expect("an invite").id;
        assert_eq!(w.party_accept(b.entity, invite, now), crate::world::PartyOutcome::Done);
        (a, b)
    }

    /// A world that queues what battles pay where the test can read it.
    fn world_with_saver(settings: BattleSettings) -> (World, mpsc::UnboundedReceiver<Msg>) {
        let mut w = world_with(Some(settings), true);
        let (handle, rx) = SaverHandle::channel();
        w.set_progress_saver(handle);
        (w, rx)
    }

    /// Everything queued for saving so far, the limit gauges too.
    fn saved_all(rx: &mut mpsc::UnboundedReceiver<Msg>) -> Vec<crate::saver::SaveJob> {
        let mut jobs = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            match msg {
                Msg::Save(job) => jobs.push(job),
                // Story saves are `saved_story`'s business (world/story.rs tests), not a payment.
                Msg::Story(_) => {}
                Msg::Stop(_) => panic!("nobody asked the saver to stop"),
            }
        }
        jobs
    }

    /// The payments and item uses queued for saving so far: what the tests of the rewards and the items are
    /// about (the limit gauges a battle ends with are queued too, see `saved_all` and `limits_of`).
    fn saved(rx: &mut mpsc::UnboundedReceiver<Msg>) -> Vec<crate::saver::SaveJob> {
        saved_all(rx).into_iter().filter(|j| !matches!(j.change, crate::saver::SavedChange::Limit(_))).collect()
    }

    /// The limit gauges among `jobs`, as `(account, character, gauge)`.
    fn limits_of(jobs: &[crate::saver::SaveJob]) -> Vec<(AccountId, u8, u16)> {
        jobs.iter()
            .filter_map(|j| match j.change {
                crate::saver::SavedChange::Limit(value) => Some((j.account, j.character, value)),
                _ => None,
            })
            .collect()
    }

    /// The `CharacterProgress` messages a seat has received so far (in order), as
    /// `(level, exp, exp_to_next, gil, ap, leveled_up)` with the character first.
    fn progress_msgs(seat: &mut Seat) -> Vec<(u8, u8, u32, u32, u32, u32, bool)> {
        seat.drain();
        seat.log
            .iter()
            .filter_map(|m| match m {
                ServerMsg::CharacterProgress { character, level, exp, exp_to_next, gil, ap, leveled_up } => {
                    Some((*character, *level, *exp, *exp_to_next, *gil, *ap, *leveled_up))
                }
                _ => None,
            })
            .collect()
    }

    /// Two players in one party.
    fn party_of_two(w: &mut World, now: Instant) -> (Seat, Seat) {
        let (a, b) = (seat(w, "A", now), seat(w, "B", now));
        assert_eq!(w.party_invite(a.entity, b.entity, now), crate::world::PartyOutcome::Done);
        let invite = w.parties.invites().next().expect("an invite").id;
        assert_eq!(w.party_accept(b.entity, invite, now), crate::world::PartyOutcome::Done);
        assert_eq!(w.party_size_of(a.account), 2);
        (a, b)
    }

    fn ms(t0: Instant, n: u64) -> Instant {
        t0 + Duration::from_millis(n)
    }

    /// Drive the world like the tick loop and the players like auto-attack bots: every 10 ms of
    /// server time advance the battles, then let every seat whose character is ready attack the
    /// first living enemy (judged from a replay of the seat's own messages, as a client would).
    /// Returns the time at which the battle ended.
    fn play_out(w: &mut World, seats: &mut [&mut Seat], rules: &SyntheticRules, t0: Instant) -> u64 {
        play_with(w, seats, rules, t0, &mut |state, _| {
            let target = state.enemy_ids().into_iter().find(|e| state.actor(*e).is_some_and(|c| c.is_alive())).unwrap();
            Command::Attack { target }
        })
    }

    /// [`play_out`] with another player: `policy` chooses the command of a ready character from the
    /// replay of what its seat was told (it must choose one the server accepts).
    fn play_with(
        w: &mut World,
        seats: &mut [&mut Seat],
        rules: &SyntheticRules,
        t0: Instant,
        policy: &mut dyn FnMut(&fmv_ff7_battle::rules::BattleState, ActorId) -> Command,
    ) -> u64 {
        let mut replays: Vec<Option<BattleReplay>> = seats.iter().map(|_| None).collect();
        let mut seen = vec![0usize; seats.len()];
        for step in 1..=60_000u64 {
            let now = ms(t0, step * 10);
            w.advance_battles(now);
            for (i, seat) in seats.iter_mut().enumerate() {
                seat.drain();
                while seen[i] < seat.log.len() {
                    match &seat.log[seen[i]] {
                        ServerMsg::EncounterStart { formation_id, seed, party, preemptive, back_attack, battle_speed, ticks_per_second, .. } => {
                            let setup = BattleSetup {
                                formation_id: *formation_id,
                                seed: *seed,
                                party: party.clone(),
                                preemptive: *preemptive,
                                back_attack: *back_attack,
                                battle_speed: *battle_speed,
                                ticks_per_second: *ticks_per_second,
                            };
                            replays[i] = Some(BattleReplay::start(rules, &setup).expect("builds"));
                        }
                        ServerMsg::BattleEvent { tick, events, command, .. } => {
                            replays[i].as_mut().expect("started").apply(*tick, events, command.as_ref()).unwrap_or_else(|d| panic!("diverged: {d}"));
                        }
                        ServerMsg::BattleEnd { outcome, rewards, .. } => {
                            replays[i].as_ref().expect("started").check_end(*outcome, rewards).unwrap_or_else(|d| panic!("end diverged: {d}"));
                            replays[i] = None;
                        }
                        _ => {}
                    }
                    seen[i] += 1;
                }
                if let Some(replay) = &replays[i] {
                    let state = replay.state();
                    let mine = ActorId(replay.party().iter().position(|m| m.entity() == Some(seat.entity)).unwrap() as u8);
                    if state.outcome().is_none() && state.ready_actors().contains(&mine) {
                        let command = policy(state, mine);
                        let id = seat_battle_id(&seat.log);
                        match w.battle_command(seat.entity, id, mine, command, now) {
                            BattleReply::Done => {}
                            BattleReply::Refused { reason, .. } => panic!("a ready actor's {command:?} was refused: {reason}"),
                            BattleReply::Gone => panic!("gone"),
                        }
                        seat.drain();
                        // The command's own message is applied by the loop above next round.
                    }
                }
            }
            if w.battle_count() == 0 && replays.iter().all(Option::is_none) {
                return step * 10;
            }
        }
        panic!("the battle never ended");
    }

    fn seat_battle_id(log: &[ServerMsg]) -> BattleId {
        log.iter()
            .rev()
            .find_map(|m| match m {
                ServerMsg::EncounterStart { battle_id, .. } => Some(*battle_id),
                _ => None,
            })
            .expect("an EncounterStart")
    }

    #[test]
    fn a_server_without_battle_data_refuses_clearly_and_a_normal_one_refuses_the_debug_request() {
        let t0 = Instant::now();
        let mut w = world_with(None, true);
        assert!(!w.battles_supported());
        let a = seat(&mut w, "A", t0);
        assert_eq!(w.request_encounter(a.entity, None, t0), refused(None, BattleRejection::Unsupported));
        assert!(!w.in_battle(a.entity));

        // Battle data, but no `--debug-encounters`.
        let mut w = world_with(Some(settings(SyntheticRules::default())), false);
        assert!(w.battles_supported());
        let a = seat(&mut w, "A", t0);
        assert_eq!(w.request_encounter(a.entity, Some(0), t0), refused(None, BattleRejection::DebugDisabled));
        assert!(!w.in_battle(a.entity));
        // A request from nobody in the world is not an answer to anyone.
        assert_eq!(w.request_encounter(EntityId(999), None, t0), BattleReply::Gone);
    }

    #[test]
    fn a_solo_request_starts_a_frozen_battle_with_the_documented_start_message() {
        let t0 = Instant::now();
        let mut w = world_with(Some(settings(SyntheticRules::default())), true);
        let mut a = seat(&mut w, "A", t0);
        let mut b = seat(&mut w, "B", t0); // a bystander in the field
        a.drain();
        b.drain();
        assert_eq!(w.request_encounter(a.entity, Some(1), t0), BattleReply::Done);
        assert!(w.in_battle(a.entity) && !w.in_battle(b.entity));
        assert_eq!(w.battle_count(), 1);

        match &a.battle_msgs()[..] {
            [ServerMsg::EncounterStart { battle_id, formation_id, seed, party, preemptive, back_attack, battle_speed, ticks_per_second, data_fingerprint }] => {
                assert_eq!((*battle_id, *formation_id, *seed), (BattleId(1), 1, 7), "the fixed seed of the settings");
                assert_eq!(party.len(), 1);
                assert_eq!((party[0].entity(), party[0].account(), party[0].character, party[0].level), (Some(a.entity), Some(a.account), 0, 5));
                assert_eq!((party[0].weapon, party[0].armor), (None, None));
                assert!(!preemptive && !back_attack);
                assert_eq!((*battle_speed, *ticks_per_second), (0, 1000));
                assert_eq!(*data_fingerprint, SyntheticRules::FINGERPRINT);
            }
            other => panic!("{other:?}"),
        }
        // The bystander hears nothing about the battle.
        assert!(b.battle_msgs().is_empty());

        // Already in a battle.
        assert_eq!(w.request_encounter(a.entity, Some(0), t0), refused(None, BattleRejection::AlreadyInBattle));
        // A formation that does not exist.
        assert_eq!(w.request_encounter(b.entity, Some(999), t0), refused(None, BattleRejection::UnknownFormation));
        assert!(!w.in_battle(b.entity), "a refused request starts nothing");
    }

    #[test]
    fn without_a_fixed_seed_every_battle_gets_its_own_seed() {
        let t0 = Instant::now();
        let mut s = settings(SyntheticRules::default());
        s.fixed_seed = None;
        let mut w = world_with(Some(s), true);
        let mut seeds = BTreeSet::new();
        for i in 0..6 {
            let mut a = seat(&mut w, &format!("S{i}"), t0);
            assert_eq!(w.request_encounter(a.entity, Some(0), t0), BattleReply::Done);
            let seed = a.battle_msgs().iter().find_map(|m| match m {
                ServerMsg::EncounterStart { seed, .. } => Some(*seed),
                _ => None,
            });
            seeds.insert(seed.expect("a start"));
        }
        assert_eq!(seeds.len(), 6, "{seeds:?}");
    }

    #[test]
    fn a_party_fights_one_shared_battle_ordered_by_entity() {
        let t0 = Instant::now();
        let mut w = world_with(Some(settings(SyntheticRules::default())), true);
        // A plays Cloud, B plays Barret (their looks, as the login found out): the character is the
        // account's own, no longer the member's place in the battle.
        let barret = ProgressSeed::of(1, None);
        let (mut a, mut b) = party_of_two_as(&mut w, t0, ProgressSeed::default(), barret);
        assert_eq!(w.party_size_of(a.account), 2);
        let mut c = seat(&mut w, "C", t0); // not in the party
        // B (the invited one) asks: the whole party goes, the bystander does not.
        assert_eq!(w.request_encounter(b.entity, None, t0), BattleReply::Done);
        assert!(w.in_battle(a.entity) && w.in_battle(b.entity) && !w.in_battle(c.entity));
        assert_eq!(w.battle_count(), 1);
        let (ma, mb) = (a.battle_msgs(), b.battle_msgs());
        assert_eq!(ma, mb, "every member hears exactly the same start");
        match &ma[..] {
            [ServerMsg::EncounterStart { formation_id, party, .. }] => {
                assert_eq!(*formation_id, 0, "no formation named: the rules' default");
                let who: Vec<(Option<EntityId>, u8)> = party.iter().map(|m| (m.entity(), m.character)).collect();
                assert_eq!(who, vec![(Some(a.entity), 0), (Some(b.entity), 1)], "ascending entity ids; each fights as its own account's character");
            }
            other => panic!("{other:?}"),
        }
        assert!(c.battle_msgs().is_empty());
        // C can start its own, separate battle.
        assert_eq!(w.request_encounter(c.entity, Some(0), t0), BattleReply::Done);
        assert_eq!(w.battle_count(), 2);
    }

    #[test]
    fn a_player_in_battle_is_frozen_and_nobody_sees_it_move() {
        let t0 = Instant::now();
        let mut w = world_with(Some(settings(SyntheticRules::default())), true);
        let mut a = seat(&mut w, "A", t0);
        let mut b = seat(&mut w, "B", t0);
        assert_eq!(w.request_encounter(a.entity, Some(0), t0), BattleReply::Done);
        a.drain();
        b.drain();
        let out = w.apply_move(a.entity, Pos::new(-90.0, 0.0, 0.0), 1.0, AnimState::Walk, ms(t0, 100));
        assert_eq!(out, MoveOutcome::Frozen);
        w.advance_tick();
        assert!(b.drain().iter().all(|m| !matches!(m, ServerMsg::EntityUpdate { .. })), "the frozen player did not move");
        // A free player still moves.
        assert_eq!(w.apply_move(b.entity, Pos::new(80.0, 0.0, 0.0), 0.0, AnimState::Walk, ms(t0, 100)), MoveOutcome::Applied);
    }

    #[test]
    fn commands_are_validated_the_actor_must_be_yours_and_ready() {
        let t0 = Instant::now();
        let mut w = world_with(Some(settings(SyntheticRules::default())), true);
        let (mut a, mut b) = party_of_two(&mut w, t0);
        assert_eq!(w.request_encounter(a.entity, Some(0), t0), BattleReply::Done);
        let id = BattleId(1);
        let enemy = ActorId(2);
        // Someone outside the battle, and a battle that does not exist.
        let c = seat(&mut w, "C", t0);
        assert_eq!(w.battle_command(c.entity, id, ActorId(0), Command::Attack { target: enemy }, t0), refused(Some(id), BattleRejection::NotInBattle));
        assert_eq!(w.battle_command(a.entity, BattleId(77), ActorId(0), Command::Attack { target: enemy }, t0), refused(Some(BattleId(77)), BattleRejection::NotInBattle));
        // B may not command A's character (and vice versa), whatever the gauges say.
        assert_eq!(w.battle_command(b.entity, id, ActorId(0), Command::Attack { target: enemy }, t0), refused(Some(id), BattleRejection::NotYourActor));
        assert_eq!(w.battle_command(a.entity, id, ActorId(1), Command::Attack { target: enemy }, t0), refused(Some(id), BattleRejection::NotYourActor));
        // Right at the start nobody's gauge is full.
        assert_eq!(w.battle_command(a.entity, id, ActorId(0), Command::Attack { target: enemy }, t0), refused(Some(id), BattleRejection::GaugeNotFull));
        // Nothing the refused commands did reached the engine: no battle event at all yet.
        assert!(a.battle_msgs().iter().all(|m| !matches!(m, ServerMsg::BattleEvent { command: Some(_), .. })));

        // Let time pass until A's character is ready (the server's own engine says so).
        let mut t = t0;
        let mut ready = false;
        for _ in 0..2_000 {
            t += Duration::from_millis(10);
            w.advance_battles(t);
            let b = w.battle.as_ref().unwrap().battles.get(&id).unwrap();
            if b.state.ready_actors().contains(&ActorId(0)) {
                ready = true;
                break;
            }
        }
        assert!(ready, "a gauge fills in the time the engine says");
        // A ready character cannot attack its own side or something that does not exist.
        assert_eq!(w.battle_command(a.entity, id, ActorId(0), Command::Attack { target: ActorId(1) }, t), refused(Some(id), BattleRejection::InvalidTarget));
        assert_eq!(w.battle_command(a.entity, id, ActorId(0), Command::Attack { target: ActorId(99) }, t), refused(Some(id), BattleRejection::InvalidTarget));
        // The real thing is applied and announced to both members with the command attached.
        assert_eq!(w.battle_command(a.entity, id, ActorId(0), Command::Attack { target: enemy }, t), BattleReply::Done);
        for seat in [&mut a, &mut b] {
            let applied = seat.battle_msgs().into_iter().rev().find_map(|m| match m {
                ServerMsg::BattleEvent { command: Some(c), events, tick, .. } => Some((c, events, tick)),
                _ => None,
            });
            let (c, events, tick) = applied.expect("the command is announced to every member");
            assert_eq!((c.actor, c.command), (ActorId(0), Command::Attack { target: enemy }));
            assert!(events.iter().any(|e| matches!(e, BattleEvent::Damage { actor: ActorId(0), .. } | BattleEvent::Miss { actor: ActorId(0), .. })));
            assert!(tick > 0);
        }
        // The gauge was spent: the same character cannot act again at once.
        assert_eq!(w.battle_command(a.entity, id, ActorId(0), Command::Attack { target: enemy }, t), refused(Some(id), BattleRejection::GaugeNotFull));
        // Only the accepted command was counted, for the actor that gave it (the end-of-battle log
        // line shows these totals; every refusal above left them alone).
        assert_eq!(w.battle.as_ref().unwrap().battles.get(&id).unwrap().commands, vec![1, 0]);
    }

    // ---- character progress (M2-d) -------------------------------------------------------------

    /// A player that joins a battle-capable server is told where its character stands as the THIRD
    /// message, after the snapshot and the placement (those two stay first, as documented).
    #[test]
    fn joining_tells_the_player_where_its_character_stands_after_the_snapshot_and_placement() {
        let t0 = Instant::now();
        let mut w = world_with(Some(settings(SyntheticRules::default())), true);
        // Level 9 starts at 144 experience on the synthetic curve and level 10 at 180: 154 is ten in.
        let stored = Progress { character: 2, level: 9, exp: 154, ap: 31, gil: 2500, limit: 0 };
        let mut a = seat_as(&mut w, "A", t0, ProgressSeed::of(2, Some(stored)));
        let msgs = a.drain();
        assert!(matches!(msgs[0], ServerMsg::AreaSnapshot { .. }), "{msgs:?}");
        assert!(matches!(msgs[1], ServerMsg::InstancePlacement { .. }), "{msgs:?}");
        assert_eq!(
            msgs[2],
            ServerMsg::CharacterProgress { character: 2, level: 9, exp: 154, exp_to_next: 26, gil: 2500, ap: 31, leveled_up: false }
        );
        assert_eq!(msgs[3], ServerMsg::Inventory { items: vec![InventoryEntry::new(0, 3), InventoryEntry::new(3, 1), InventoryEntry::new(7, 1)] });
        assert_eq!(msgs.len(), 4, "{msgs:?}");

        // A new account starts where the rules say: level 5 with the experience of level 5 (40).
        let mut b = seat(&mut w, "B", t0);
        assert_eq!(progress_msgs(&mut b), vec![(0, 5, 40, 20, 0, 0, false)]);
    }

    #[test]
    fn a_server_that_runs_no_battles_tells_nobody_anything_about_progress() {
        let t0 = Instant::now();
        let mut w = world_with(None, true);
        let mut a = seat(&mut w, "A", t0);
        assert!(progress_msgs(&mut a).is_empty());
        assert!(inventory_msgs(&mut a).is_empty(), "no items on a server without battle data");
        assert!(w.players.values().all(|p| p.progress.is_none() && p.inventory.is_none()));
    }

    // ---- inventory (M2-e) ----

    /// The `Inventory` messages a seat has received so far, in order, as `(item, count)` lists.
    fn inventory_msgs(seat: &mut Seat) -> Vec<Vec<(u16, u16)>> {
        seat.drain();
        seat.log
            .iter()
            .filter_map(|m| match m {
                ServerMsg::Inventory { items } => Some(items.iter().map(|e| (e.item, e.count)).collect()),
                _ => None,
            })
            .collect()
    }

    /// Run the clock of battle `id` until `actor` can act; returns the time it got to.
    fn until_ready(w: &mut World, id: BattleId, actor: ActorId, from: Instant) -> Instant {
        let mut t = from;
        for _ in 0..5_000 {
            t += Duration::from_millis(10);
            w.advance_battles(t);
            if w.battle.as_ref().and_then(|b| b.battles.get(&id)).is_some_and(|b| b.state.ready_actors().contains(&actor)) {
                return t;
            }
        }
        panic!("{actor:?} never became ready");
    }

    /// 3 Potions, an Ether and a Phoenix Down: what `SyntheticRules` starts an account with.
    fn stock() -> Vec<(u16, u16)> {
        vec![(0, 3), (3, 1), (7, 1)]
    }

    /// A login that found this inventory in the store.
    fn seeded_with(items: &[(u16, u16)]) -> ProgressSeed {
        ProgressSeed { inventory: Some(items.iter().map(|&(item, count)| InventoryEntry::new(item, count)).collect()), ..ProgressSeed::default() }
    }

    /// The server's mirror of what a player carries.
    fn mirror_of(w: &World, who: &Seat) -> Vec<(u16, u16)> {
        let inventory = w.players.get(&who.entity).and_then(|p| p.inventory.as_ref());
        inventory.map(|i| i.entries().iter().map(|e| (e.item, e.count)).collect()).unwrap_or_default()
    }

    #[test]
    fn a_new_account_carries_the_starting_stock_and_a_stored_inventory_replaces_it() {
        let t0 = Instant::now();
        let mut w = world_with(Some(settings(SyntheticRules::default())), true);
        let mut a = seat(&mut w, "A", t0);
        assert_eq!(inventory_msgs(&mut a), vec![stock()]);
        assert_eq!(mirror_of(&w, &a), stock());
        // Right after the progress, which is right after the snapshot and the placement.
        let order: Vec<&str> = a
            .log
            .iter()
            .filter_map(|m| match m {
                ServerMsg::AreaSnapshot { .. } => Some("snapshot"),
                ServerMsg::InstancePlacement { .. } => Some("placement"),
                ServerMsg::CharacterProgress { .. } => Some("progress"),
                ServerMsg::Inventory { .. } => Some("inventory"),
                _ => None,
            })
            .collect();
        assert_eq!(order, ["snapshot", "placement", "progress", "inventory"]);
        // A stored inventory is what the player has, and an empty one is empty: the stock is given once.
        let mut b = seat_as(&mut w, "B", t0, seeded_with(&[(7, 2)]));
        assert_eq!(inventory_msgs(&mut b), vec![vec![(7, 2)]]);
        let mut c = seat_as(&mut w, "C", t0, seeded_with(&[]));
        assert_eq!(inventory_msgs(&mut c), vec![Vec::<(u16, u16)>::new()]);
        // What is stored is brought inside the rules before it is believed or sent.
        let mut d = seat_as(&mut w, "D", t0, seeded_with(&[(5, 60_000), (1, 0), (5, 3)]));
        assert_eq!(inventory_msgs(&mut d), vec![vec![(5, 99)]]);
    }

    #[test]
    fn an_item_is_spent_told_and_saved_only_when_the_engine_accepts_the_command() {
        let t0 = Instant::now();
        let (mut w, mut rx) = world_with_saver(settings(SyntheticRules::default()));
        // 3 Potions, an Ether, a Phoenix Down, and 5 of an item (8) that the engine cannot use in battle.
        let mut a = seat_as(&mut w, "A", t0, seeded_with(&[(0, 3), (3, 1), (7, 1), (8, 5)]));
        assert_eq!(w.request_encounter(a.entity, Some(0), t0), BattleReply::Done);
        a.drain();
        let id = BattleId(1);
        let t = until_ready(&mut w, id, ActorId(0), t0);
        let item = |item, target: u8| Command::Item { item, target: Target::One(ActorId(target)) };
        let before = mirror_of(&w, &a);

        // What the engine refuses is refused as the engine says, whatever the account owns, and costs nothing.
        for (command, reason) in [
            (item(8, 0), BattleRejection::UnknownItem),
            (item(99, 0), BattleRejection::UnknownItem),
            (item(0, 1), BattleRejection::InvalidTarget),
            (item(0, 9), BattleRejection::InvalidTarget),
            (item(20, 0), BattleRejection::InvalidTarget),
            (item(7, 0), BattleRejection::TargetNotDead),
        ] {
            assert_eq!(w.battle_command(a.entity, id, ActorId(0), command, t), refused(Some(id), reason), "{command:?}");
        }
        assert_eq!(mirror_of(&w, &a), before);
        assert!(saved(&mut rx).is_empty(), "a refused command saves nothing");
        assert!(
            a.drain().iter().all(|m| !matches!(m, ServerMsg::Inventory { .. } | ServerMsg::BattleEvent { command: Some(_), .. })),
            "a refused command tells nobody anything"
        );

        // A Potion on itself is accepted: the event that applied it, then the inventory, right after.
        assert_eq!(w.battle_command(a.entity, id, ActorId(0), item(0, 0), t), BattleReply::Done);
        let msgs = a.drain();
        let applied = msgs
            .iter()
            .position(|m| matches!(m, ServerMsg::BattleEvent { command: Some(AppliedCommand { command: Command::Item { item: 0, .. }, .. }), .. }))
            .expect("the command is announced");
        let want = vec![(0, 2), (3, 1), (7, 1), (8, 5)];
        assert_eq!(msgs[applied + 1], ServerMsg::Inventory { items: want.iter().map(|&(i, c)| InventoryEntry::new(i, c)).collect() });
        assert_eq!(msgs.len(), applied + 2, "{msgs:?}");
        assert_eq!(mirror_of(&w, &a), want);
        assert_eq!(saved(&mut rx), vec![crate::saver::SaveJob::used_item(a.account, 0, 0)]);
        let logs: Vec<String> = w.take_logs().iter().map(|l| l.text().to_string()).collect();
        let line = logs.iter().find(|l| l.starts_with("item used")).expect("the log says it");
        assert!(
            line.contains(&format!("battle=1 account={} character=0 item=0 left=2 saving=queued", a.account.0)),
            "{line}"
        );
    }

    /// Protocol v10: a command that hits a whole side is one command. A Megalixir restores the whole party and
    /// is one item taken; Fire on every enemy costs the MP once and the caster's one All use, and a second
    /// whole-side spell is refused without costing the turn; Shrapnel hits every enemy, one item taken.
    #[test]
    fn a_whole_side_command_is_one_command_one_item_and_one_all_use() {
        let t0 = Instant::now();
        let (mut w, mut rx) = world_with_saver(settings(SyntheticRules::default()));
        let mut a = seat_as(&mut w, "A", t0, seeded_with(&[(6, 2), (21, 1)]));
        // Formation 1: two weak enemies (200 HP each).
        assert_eq!(w.request_encounter(a.entity, Some(1), t0), BattleReply::Done);
        a.drain();
        let id = BattleId(1);
        let mut t = until_ready(&mut w, id, ActorId(0), t0);
        let me = ActorId(0);
        let damaged = |msgs: &[ServerMsg]| -> Vec<ActorId> {
            msgs.iter()
                .filter_map(|m| match m {
                    ServerMsg::BattleEvent { events, .. } => Some(events),
                    _ => None,
                })
                .flatten()
                .filter_map(|e| if let BattleEvent::Damage { actor, target, .. } = e { (*actor == me).then_some(*target) } else { None })
                .collect()
        };

        // Megalixir on the whole party: one command, one Megalixir.
        let wind = Command::Item { item: 6, target: Target::AllAllies };
        assert_eq!(w.battle_command(a.entity, id, me, wind, t), BattleReply::Done);
        let msgs = a.drain();
        let healed: Vec<ActorId> = msgs
            .iter()
            .filter_map(|m| if let ServerMsg::BattleEvent { events, .. } = m { Some(events) } else { None })
            .flatten()
            .filter_map(|e| if let BattleEvent::Heal { target, .. } = e { Some(*target) } else { None })
            .collect();
        assert_eq!(healed, [me], "a party of one: the one ally");
        assert_eq!(mirror_of(&w, &a), vec![(6, 1), (21, 1)]);
        assert_eq!(saved(&mut rx), vec![crate::saver::SaveJob::used_item(a.account, 0, 6)]);
        let lines = logs_of(&mut w);
        assert!(lines.iter().any(|l| l.starts_with("item used") && l.contains("item=6 left=1 saving=queued")), "{lines:?}");

        // Fire on every enemy: both hit, once the MP and the one All use.
        t = until_ready(&mut w, id, me, t);
        let fire = |target| Command::Magic { spell: 27, target };
        assert_eq!(w.battle_command(a.entity, id, me, fire(Target::AllEnemies), t), BattleReply::Done);
        assert_eq!(damaged(&a.drain()), [ActorId(1), ActorId(2)], "every enemy, in actor order");
        let lines = logs_of(&mut w);
        assert!(lines.iter().any(|l| l.starts_with("magic used") && logged(l, "spell") == Some("27") && logged(l, "target") == Some("all-enemies")), "{lines:?}");
        // The All use is spent: a second whole-side spell is refused and the turn is not, one target is fine.
        t = until_ready(&mut w, id, me, t);
        assert_eq!(w.battle_command(a.entity, id, me, fire(Target::AllEnemies), t), refused(Some(id), BattleRejection::InvalidTarget));
        assert!(w.battle.as_ref().unwrap().battles.get(&id).unwrap().state.ready_actors().contains(&me), "a refusal spends no turn");
        assert!(a.drain().iter().all(|m| !matches!(m, ServerMsg::BattleEvent { command: Some(_), .. })));

        // Shrapnel on every enemy: one item, and the two enemies (155 + the fire) are finished with it.
        let shrapnel = Command::Item { item: 21, target: Target::AllEnemies };
        assert_eq!(w.battle_command(a.entity, id, me, shrapnel, t), BattleReply::Done);
        let msgs = a.drain();
        assert_eq!(damaged(&msgs), [ActorId(1), ActorId(2)]);
        assert!(msgs.iter().any(|m| matches!(m, ServerMsg::BattleEnd { outcome: Outcome::Victory, .. })), "{msgs:?}");
        assert_eq!(mirror_of(&w, &a), vec![(6, 1)]);
        assert!(saved(&mut rx).iter().any(|j| *j == crate::saver::SaveJob::used_item(a.account, 0, 21)));
    }

    #[test]
    fn an_account_without_the_item_is_refused_even_when_the_engine_would_accept_it() {
        let t0 = Instant::now();
        let (mut w, mut rx) = world_with_saver(settings(SyntheticRules::default()));
        // Only an Ether.
        let mut a = seat_as(&mut w, "A", t0, seeded_with(&[(3, 1)]));
        assert_eq!(w.request_encounter(a.entity, Some(0), t0), BattleReply::Done);
        a.drain();
        let id = BattleId(1);
        let mut t = until_ready(&mut w, id, ActorId(0), t0);
        let item = |item| Command::Item { item, target: Target::One(ActorId(0)) };
        let still_ready = |w: &World| w.battle.as_ref().unwrap().battles.get(&id).unwrap().state.ready_actors().contains(&ActorId(0));
        // A Potion is something the engine accepts, and the account has none: refused, the turn is not spent.
        assert_eq!(w.battle_command(a.entity, id, ActorId(0), item(0), t), refused(Some(id), BattleRejection::OutOfItems));
        assert!(still_ready(&w));
        assert_eq!(w.battle.as_ref().unwrap().battles.get(&id).unwrap().commands, vec![0]);
        assert!(saved(&mut rx).is_empty());
        // The Ether it has goes, and the stack is gone from what it carries; the same again is refused.
        assert_eq!(w.battle_command(a.entity, id, ActorId(0), item(3), t), BattleReply::Done);
        assert_eq!(inventory_msgs(&mut a).last(), Some(&Vec::<(u16, u16)>::new()));
        assert_eq!(mirror_of(&w, &a), Vec::<(u16, u16)>::new());
        t = until_ready(&mut w, id, ActorId(0), t);
        assert_eq!(w.battle_command(a.entity, id, ActorId(0), item(3), t), refused(Some(id), BattleRejection::OutOfItems));
        assert_eq!(saved(&mut rx).len(), 1, "one Ether, one saved change");
        // Spells and Defend need no item and touch no inventory.
        assert_eq!(w.battle_command(a.entity, id, ActorId(0), Command::Magic { spell: 27, target: Target::One(ActorId(1)) }, t), BattleReply::Done);
        t = until_ready(&mut w, id, ActorId(0), t);
        assert_eq!(w.battle_command(a.entity, id, ActorId(0), Command::Defend, t), BattleReply::Done);
        assert!(saved(&mut rx).is_empty());
        // The refusals of spells are the engine's too.
        t = until_ready(&mut w, id, ActorId(0), t);
        for (command, reason) in [
            (Command::Magic { spell: 99, target: Target::One(ActorId(1)) }, BattleRejection::UnknownSpell),
            (Command::Magic { spell: 27, target: Target::One(ActorId(0)) }, BattleRejection::InvalidTarget),
            (Command::Magic { spell: 7, target: Target::One(ActorId(0)) }, BattleRejection::TargetNotDead),
        ] {
            assert_eq!(w.battle_command(a.entity, id, ActorId(0), command, t), refused(Some(id), reason), "{command:?}");
        }
    }

    #[test]
    fn a_command_that_wins_the_battle_still_spends_its_item_before_the_end() {
        let t0 = Instant::now();
        // One Grenade (about 160 damage) kills a 100 HP dummy.
        let rules = SyntheticRules { enemy_hp: 100, ..SyntheticRules::default() };
        let (mut w, mut rx) = world_with_saver(settings(rules));
        let mut a = seat_as(&mut w, "A", t0, seeded_with(&[(20, 2)]));
        assert_eq!(w.request_encounter(a.entity, Some(0), t0), BattleReply::Done);
        a.drain();
        let id = BattleId(1);
        let t = until_ready(&mut w, id, ActorId(0), t0);
        a.drain(); // the gauge-ready event of the wait
        assert_eq!(w.battle_command(a.entity, id, ActorId(0), Command::Item { item: 20, target: Target::One(ActorId(1)) }, t), BattleReply::Done);
        let kinds: Vec<&str> = a
            .drain()
            .iter()
            .filter_map(|m| match m {
                ServerMsg::BattleEvent { .. } => Some("event"),
                ServerMsg::Inventory { .. } => Some("inventory"),
                ServerMsg::BattleEnd { .. } => Some("end"),
                ServerMsg::CharacterProgress { .. } => Some("progress"),
                _ => None,
            })
            .collect();
        assert_eq!(kinds, ["event", "inventory", "end", "progress"], "the item is spent before the battle ends");
        assert_eq!(mirror_of(&w, &a), vec![(20, 1)]);
        let jobs = saved(&mut rx);
        assert_eq!(jobs.len(), 2, "the Grenade, and the win: {jobs:?}");
        assert_eq!(jobs[0], crate::saver::SaveJob::used_item(a.account, 0, 20));
        assert!(jobs[1].paid().is_some());
    }

    #[test]
    fn a_win_drops_items_to_the_living_tells_them_after_their_progress_and_queues_them() {
        let t0 = Instant::now();
        // Every dummy drops a Potion: formation 1 has two, so one each for the two living members.
        let rules = SyntheticRules { drop_item: Some(0), ..SyntheticRules::default() };
        let (mut w, mut rx) = world_with_saver(settings(rules));
        let (mut a, mut b) = party_of_two(&mut w, t0);
        a.drain();
        b.drain();
        assert_eq!(w.request_encounter(a.entity, Some(1), t0), BattleReply::Done);
        // play_out replays every message and checks the end with `BattleReplay::check_end`, which does not
        // compare the drops: a replay cannot reproduce them.
        play_out(&mut w, &mut [&mut a, &mut b], &rules, t0);
        for seat in [&mut a, &mut b] {
            let tail: Vec<&str> = seat
                .log
                .iter()
                .filter_map(|m| match m {
                    ServerMsg::BattleEnd { rewards, .. } => {
                        assert!(rewards.iter().all(|r| r.items == vec![InventoryEntry::new(0, 1)]), "{rewards:?}");
                        Some("end")
                    }
                    ServerMsg::CharacterProgress { .. } => Some("progress"),
                    ServerMsg::Inventory { .. } => Some("inventory"),
                    _ => None,
                })
                .collect();
            assert_eq!(tail, ["progress", "inventory", "end", "progress", "inventory"], "join, then the end, the payment and the drops");
            assert_eq!(inventory_msgs(seat).last(), Some(&vec![(0, 4), (3, 1), (7, 1)]), "3 Potions and the one dropped");
        }
        assert_eq!(mirror_of(&w, &a), vec![(0, 4), (3, 1), (7, 1)]);
        assert_eq!(mirror_of(&w, &b), vec![(0, 4), (3, 1), (7, 1)]);
        // Both payments carry the item to the saver.
        let jobs = saved(&mut rx);
        assert_eq!(jobs.len(), 2);
        assert!(jobs.iter().all(|j| j.paid().is_some_and(|r| r.items == vec![InventoryEntry::new(0, 1)])), "{jobs:?}");
        // The roll is logged with the formation and the seed it came from.
        let logs: Vec<String> = w.take_logs().iter().map(|l| l.text().to_string()).collect();
        assert!(logs.iter().any(|l| l == "battle drops battle=1 formation=1 seed=7 items=[(0, 2)]"), "{logs:?}");
        assert!(logs.iter().any(|l| l.starts_with("battle reward") && l.contains("dropped=[(0, 1)]")), "{logs:?}");
    }

    /// A member that is dead at the end gets nothing (not even a message about items), the living one gets
    /// the drop, and the store hears of the living one's items only.
    #[test]
    fn drops_go_to_the_living_member_and_a_dead_one_gets_nothing() {
        let t0 = Instant::now();
        let (mut w, mut rx) = world_with_saver(settings(SyntheticRules::default()));
        let (mut a, mut b) = party_of_two(&mut w, t0);
        a.drain();
        b.drain();
        let members: Vec<Member> = [(&a, 0u8), (&b, 1u8)]
            .iter()
            .map(|(seat, character)| Member {
                info: BattleMember::player(seat.entity, seat.account, "Tester", *character, 5, 0),
                connected: true,
            })
            .collect();
        let mut rewards = vec![BattleReward::new(a.account, 24, 40, 4), BattleReward::new(b.account, 0, 0, 0)];
        rewards[0].items = vec![InventoryEntry::new(0, 2), InventoryEntry::new(7, 1)];
        let told = w.pay_members(BattleId(1), &members, &rewards);
        let kinds: Vec<(EntityId, &str)> = told
            .iter()
            .map(|(e, m)| (*e, if matches!(**m, ServerMsg::Inventory { .. }) { "inventory" } else { "progress" }))
            .collect();
        assert_eq!(kinds, vec![(a.entity, "progress"), (a.entity, "inventory"), (b.entity, "progress")]);
        assert_eq!(mirror_of(&w, &a), vec![(0, 5), (3, 1), (7, 2)]);
        assert_eq!(mirror_of(&w, &b), stock(), "the dead member's items are what they were");
        let jobs = saved(&mut rx);
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].paid().map(|r| r.items.clone()), Some(vec![InventoryEntry::new(0, 2), InventoryEntry::new(7, 1)]));
        // A payment of items alone is a payment, whatever the numbers are.
        let mut only_items = vec![BattleReward::new(b.account, 0, 0, 0)];
        only_items[0].items = vec![InventoryEntry::new(3, 1)];
        let told = w.pay_members(BattleId(2), &members, &only_items);
        assert_eq!(told.len(), 2, "its progress and its inventory");
        assert_eq!(mirror_of(&w, &b), vec![(0, 3), (3, 2), (7, 1)]);
        assert_eq!(saved(&mut rx).len(), 1);
    }

    /// A member that left mid-battle fought on: the drops reach the store (nobody is online to be told).
    #[test]
    fn a_member_who_left_is_handed_its_drops_in_the_store_too() {
        let t0 = Instant::now();
        let rules = SyntheticRules { drop_item: Some(3), ..SyntheticRules::default() };
        let (mut w, mut rx) = world_with_saver(settings(rules));
        let (mut a, b) = party_of_two(&mut w, t0);
        assert_eq!(w.request_encounter(a.entity, Some(1), t0), BattleReply::Done);
        let (b_account, b_entity) = (b.account, b.entity);
        assert!(w.leave(b_entity));
        drop(b);
        play_out(&mut w, &mut [&mut a], &rules, t0);
        let jobs = saved(&mut rx);
        let b_items: Option<Vec<InventoryEntry>> = jobs.iter().find(|j| j.account == b_account).and_then(|j| j.paid()).map(|r| r.items.clone());
        let a_items: Option<Vec<InventoryEntry>> = jobs.iter().find(|j| j.account == a.account).and_then(|j| j.paid()).map(|r| r.items.clone());
        // Two Ethers, dealt one to each of the two living members of the roster.
        assert_eq!(b_items, Some(vec![InventoryEntry::new(3, 1)]), "{jobs:?}");
        assert_eq!(a_items, Some(vec![InventoryEntry::new(3, 1)]), "{jobs:?}");
        assert_eq!(mirror_of(&w, &a), vec![(0, 3), (3, 2), (7, 1)]);
    }

    /// An item command from a player whose session is gone by the time the engine takes it still spends
    /// the item in the store: the engine already acted.
    #[test]
    fn the_item_a_command_spent_is_saved_even_if_the_player_is_dropped_meanwhile() {
        let t0 = Instant::now();
        let (mut w, mut rx) = world_with_saver(settings(SyntheticRules::default()));
        let a = seat(&mut w, "A", t0);
        // A queue too small to hold what the battle says: the first fan-out of the command drops the player.
        // (Seats have a 4096-message queue, so simulate the drop directly.)
        assert_eq!(w.request_encounter(a.entity, Some(0), t0), BattleReply::Done);
        let id = BattleId(1);
        let t = until_ready(&mut w, id, ActorId(0), t0);
        let (account, entity) = (a.account, a.entity);
        assert_eq!(w.battle_command(entity, id, ActorId(0), Command::Item { item: 0, target: Target::One(ActorId(0)) }, t), BattleReply::Done);
        assert_eq!(saved(&mut rx), vec![crate::saver::SaveJob::used_item(account, 0, 0)]);
        // `spend_item` for an entity that is no longer there still queues the change and says nothing to anyone.
        w.spend_item(id, EntityId(999), account, 0, 0);
        assert_eq!(saved(&mut rx), vec![crate::saver::SaveJob::used_item(account, 0, 0)]);
    }

    #[test]
    fn every_member_fights_as_its_own_character_at_its_own_level() {
        let t0 = Instant::now();
        let mut w = world_with(Some(settings(SyntheticRules::default())), true);
        let veteran = Progress { character: 0, level: 12, exp: 270, ap: 0, gil: 0, limit: 0 }; // level 12 starts at 264
        let (mut a, mut b) = party_of_two_as(
            &mut w,
            t0,
            ProgressSeed::of(0, Some(veteran)),
            ProgressSeed::of(2, None), // Tifa, never earned anything: level 5
        );
        assert_eq!(w.request_encounter(a.entity, Some(0), t0), BattleReply::Done);
        let start = a.battle_msgs();
        assert_eq!(start, b.battle_msgs());
        match &start[..] {
            [ServerMsg::EncounterStart { party, .. }] => {
                let who: Vec<(Option<EntityId>, u8, u8)> = party.iter().map(|m| (m.entity(), m.character, m.level)).collect();
                assert_eq!(who, vec![(Some(a.entity), 0, 12), (Some(b.entity), 2, 5)]);
            }
            other => panic!("{other:?}"),
        }
    }

    /// The roster carries the PLAYERS' OWN names (v8), whatever combat template they fight with: two players may hold
    /// the same slot, and the log says which player and which template, with a name that cannot split a line.
    #[test]
    fn the_roster_carries_the_players_own_names_and_the_slot_only_picks_the_template() {
        let t0 = Instant::now();
        let mut w = world_with(Some(settings(SyntheticRules::default())), true);
        // Both hold Barret's slot (1), as two new characters may; one is "A", the other has a space in its name.
        let a = seat_as(&mut w, "A", t0, ProgressSeed::of(1, None));
        let b = seat_as(&mut w, "Red XIII", t0, ProgressSeed::of(1, None));
        assert_eq!(w.party_invite(a.entity, b.entity, t0), crate::world::PartyOutcome::Done);
        let invite = w.parties.invites().next().expect("an invite").id;
        assert_eq!(w.party_accept(b.entity, invite, t0), crate::world::PartyOutcome::Done);
        let (mut a, mut b) = (a, b);
        assert_eq!(w.request_encounter(a.entity, Some(0), t0), BattleReply::Done);
        let start = a.battle_msgs();
        assert_eq!(start, b.battle_msgs());
        match &start[..] {
            [ServerMsg::EncounterStart { party, .. }] => {
                let who: Vec<(&str, u8)> = party.iter().map(|m| (m.name.as_str(), m.character)).collect();
                assert_eq!(who, vec![("A", 1), ("Red XIII", 1)], "their own names, the same template");
            }
            other => panic!("{other:?}"),
        }
        let logs = logs_of(&mut w);
        assert!(
            logs.iter().any(|l| l.starts_with("battle started battle=1") && l.contains("characters=[1, 1]") && l.contains("names=[A,Red%20XIII] companions=0 statuses=[-,-]")),
            "{logs:?}"
        );
    }

    /// `--battle-level` pins the fight for everybody; what the accounts have earned is untouched.
    #[test]
    fn a_pinned_battle_level_pins_the_fight_but_not_the_account() {
        let t0 = Instant::now();
        let rules = SyntheticRules::default();
        let mut s = settings(rules);
        s.level = Some(3);
        let (mut w, mut rx) = world_with_saver(s);
        let veteran = Progress { character: 0, level: 12, exp: 270, ap: 0, gil: 0, limit: 0 }; // level 12 starts at 264, 13 at 312
        let mut a = seat_as(&mut w, "A", t0, ProgressSeed::of(0, Some(veteran)));
        assert_eq!(w.request_encounter(a.entity, Some(0), t0), BattleReply::Done);
        match &a.battle_msgs()[..] {
            [ServerMsg::EncounterStart { party, .. }] => assert_eq!((party[0].character, party[0].level), (0, 3)),
            other => panic!("{other:?}"),
        }
        play_out(&mut w, &mut [&mut a], &rules, t0);
        // Won at level 3, paid to the account that is level 12 (formation 0: one dummy, 12 experience,
        // 20 gil, 2 AP): 270 + 12 = 282, 30 short of level 13.
        assert_eq!(*progress_msgs(&mut a).last().unwrap(), (0, 12, 282, 30, 20, 2, false));
        assert_eq!(saved(&mut rx).len(), 1);
    }

    #[test]
    fn a_victory_pays_the_member_in_its_mirror_in_a_message_after_the_end_and_in_the_saver_queue() {
        let rules = SyntheticRules::default();
        let t0 = Instant::now();
        let (mut w, mut rx) = world_with_saver(settings(rules));
        let mut a = seat(&mut w, "A", t0); // Cloud, level 5, 40 experience: level 6 at 60, level 7 at 84
        assert_eq!(progress_msgs(&mut a), vec![(0, 5, 40, 20, 0, 0, false)]);

        // Formation 0 is one dummy: 12 experience, 20 gil, 2 AP. 40 + 12 = 52: no level yet.
        assert_eq!(w.request_encounter(a.entity, Some(0), t0), BattleReply::Done);
        play_out(&mut w, &mut [&mut a], &rules, t0);
        let tail: Vec<&'static str> = a
            .log
            .iter()
            .map(|m| match m {
                ServerMsg::BattleEnd { .. } => "end",
                ServerMsg::CharacterProgress { .. } => "progress",
                _ => "",
            })
            .filter(|n| !n.is_empty())
            .collect();
        assert_eq!(tail, ["progress", "end", "progress"], "the join message, then the end, then what it paid: {:?}", a.log.len());
        assert_eq!(progress_msgs(&mut a).last(), Some(&(0, 5, 52, 8, 20, 2, false)));
        assert_eq!(
            saved(&mut rx),
            vec![crate::saver::SaveJob::reward(a.account, 0, fmv_protocol::BattleReward::new(a.account, 12, 20, 2))]
        );

        // Formation 1 is two dummies: 24 experience, 40 gil, 4 AP. 52 + 24 = 76: level 6 (60), the
        // message says so and `exp_to_next` counts towards level 7 (84).
        let later = ms(t0, 60_000 * 10 + 1_000);
        assert_eq!(w.request_encounter(a.entity, Some(1), later), BattleReply::Done);
        // The mirror's level is on the wire of the next start: the progress is not only a message.
        match a.battle_msgs().last() {
            Some(ServerMsg::EncounterStart { party, .. }) => assert_eq!((party[0].character, party[0].level), (0, 5), "still level 5 before this win"),
            other => panic!("{other:?}"),
        }
        play_out(&mut w, &mut [&mut a], &rules, later);
        assert_eq!(progress_msgs(&mut a).last(), Some(&(0, 6, 76, 8, 60, 6, true)));
        assert_eq!(saved(&mut rx).len(), 1);

        // And the battle after that fights at level 6.
        let much_later = ms(later, 60_000 * 10 + 1_000);
        assert_eq!(w.request_encounter(a.entity, Some(0), much_later), BattleReply::Done);
        match a.battle_msgs().last() {
            Some(ServerMsg::EncounterStart { party, .. }) => assert_eq!(party[0].level, 6, "the level gained is the level fought at"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn every_member_that_fought_on_is_paid_its_own_share_into_its_own_progress() {
        let rules = SyntheticRules::default();
        let t0 = Instant::now();
        let (mut w, mut rx) = world_with_saver(settings(rules));
        // B has 8 experience to go to level 6, A has 20.
        let (mut a, mut b) = party_of_two_as(
            &mut w,
            t0,
            ProgressSeed::of(0, None),
            ProgressSeed::of(3, Some(Progress { character: 3, level: 5, exp: 52, ap: 1, gil: 5, limit: 0 })),
        );
        a.drain();
        b.drain();
        // Two dummies: 24 experience, 40 gil, 4 AP, split 12 / 20 / 4 over two living members.
        assert_eq!(w.request_encounter(a.entity, Some(1), t0), BattleReply::Done);
        play_out(&mut w, &mut [&mut a, &mut b], &rules, t0);
        let (pa, pb) = (progress_msgs(&mut a), progress_msgs(&mut b));
        // Whoever lived got a message; the numbers are the account's own plus a share.
        let paid = |p: &[(u8, u8, u32, u32, u32, u32, bool)], start: (u32, u32, u32)| {
            p.last().map(|&(_, _, exp, _, gil, ap, _)| (exp - start.0, gil - start.1, ap - start.2))
        };
        assert_eq!(paid(&pa, (40, 0, 0)), Some((12, 20, 4)), "{pa:?}");
        assert_eq!(paid(&pb, (52, 5, 1)), Some((12, 20, 4)), "{pb:?}");
        // B crossed level 6 (60) with 52 + 12 = 64, A did not (40 + 12 = 52).
        assert_eq!(pb.last().map(|p| (p.1, p.6)), Some((6, true)));
        assert_eq!(pa.last().map(|p| (p.1, p.6)), Some((5, false)));
        // One queue entry per payment, in roster order, each for the character that fought.
        let jobs = saved(&mut rx);
        assert_eq!(jobs.iter().map(|j| (j.account, j.character, j.paid().map(|r| r.exp))).collect::<Vec<_>>(), vec![(a.account, 0, Some(12)), (b.account, 3, Some(12))]);
    }

    /// What a client that replayed everything `seat` was told computes as the limit gauges of the party
    /// (`ActorId(0)` first) at the end of its last battle: an oracle for what the server saves.
    fn replayed_gauges(seat: &mut Seat, rules: &SyntheticRules) -> Vec<u16> {
        seat.drain();
        let mut replay = None;
        for m in &seat.log {
            match m {
                ServerMsg::EncounterStart { formation_id, seed, party, preemptive, back_attack, battle_speed, ticks_per_second, .. } => {
                    let setup = BattleSetup {
                        formation_id: *formation_id,
                        seed: *seed,
                        party: party.clone(),
                        preemptive: *preemptive,
                        back_attack: *back_attack,
                        battle_speed: *battle_speed,
                        ticks_per_second: *ticks_per_second,
                    };
                    replay = Some(BattleReplay::start(rules, &setup).expect("builds"));
                }
                ServerMsg::BattleEvent { tick, events, command, .. } => {
                    replay.as_mut().expect("started").apply(*tick, events, command.as_ref()).unwrap_or_else(|d| panic!("diverged: {d}"));
                }
                _ => {}
            }
        }
        let replay = replay.expect("a battle");
        let state = replay.state();
        state.party_ids().into_iter().map(|id| state.limit_gauge(id)).collect()
    }

    fn logs_of(w: &mut World) -> Vec<String> {
        w.take_logs().iter().map(|l| l.text().to_string()).collect()
    }

    /// The value of `key` in a `key=value` log line (the lines end in `name=..`, so nothing is read from the end of one).
    fn logged<'a>(line: &'a str, key: &str) -> Option<&'a str> {
        line.split(' ').find_map(|word| word.strip_prefix(key)?.strip_prefix('='))
    }

    /// The limit gauge of `seat`'s mirror.
    fn mirrored_limit(w: &World, seat: &Seat) -> u16 {
        w.players.get(&seat.entity).and_then(|p| p.progress).expect("a mirror").limit
    }

    #[test]
    fn a_member_starts_with_its_accounts_gauge_uses_its_limit_and_the_end_gauge_is_kept() {
        let rules = player_limit_rules();
        let t0 = Instant::now();
        let (mut w, mut rx) = world_with_saver(settings(rules));
        let stored = Progress { character: 0, level: 5, exp: 40, ap: 0, gil: 0, limit: 255 };
        let mut a = seat_as(&mut w, "A", t0, ProgressSeed::of(0, Some(stored)));
        assert_eq!(mirrored_limit(&w, &a), 255);
        // Formation 3 is the preemptive one: the party's gauge is full at once and the limit can be given.
        assert_eq!(w.request_encounter(a.entity, Some(3), t0), BattleReply::Done);
        match &a.battle_msgs()[..] {
            [ServerMsg::EncounterStart { party, preemptive: true, .. }] => assert_eq!((party[0].character, party[0].limit), (0, 255)),
            other => panic!("{other:?}"),
        }
        let id = seat_battle_id(&a.log);
        assert_eq!(w.battle_command(a.entity, id, ActorId(0), Command::Limit { target: Target::One(ActorId(1)) }, t0), BattleReply::Done);
        // The message that carried it: the action, the gauge emptied before the blow, then the blow.
        let applied = a.battle_msgs().into_iter().find_map(|m| match m {
            ServerMsg::BattleEvent { events, command: Some(AppliedCommand { command: Command::Limit { .. }, .. }), .. } => Some(events),
            _ => None,
        });
        let events = applied.expect("the limit was announced with its command");
        assert!(matches!(events.last(), Some(BattleEvent::Damage { .. })), "{events:?}");
        assert!(events.contains(&BattleEvent::Action { actor: ActorId(0), action: fmv_protocol::ActionRef::Limit(128) }));
        assert!(events.contains(&BattleEvent::LimitGauge { actor: ActorId(0), value: 0 }));
        // The same gauge a second time: the turn is over (and the gauge is empty).
        assert_eq!(
            w.battle_command(a.entity, id, ActorId(0), Command::Limit { target: Target::One(ActorId(1)) }, t0),
            refused(Some(id), BattleRejection::GaugeNotFull)
        );

        play_out(&mut w, &mut [&mut a], &rules, t0);
        // The gauge it ended with is what a replay of the log computes: queued for the saver (a gauge that
        // is not the one the battle started with), in the mirror, and logged.
        let end = replayed_gauges(&mut a, &rules)[0];
        assert_ne!(end, 255, "the limit emptied it and a dummy's few blows did not fill it again");
        let jobs = saved_all(&mut rx);
        assert_eq!(limits_of(&jobs), vec![(a.account, 0, end)]);
        assert_eq!(mirrored_limit(&w, &a), end);
        let logs = logs_of(&mut w);
        assert!(
            logs.iter().any(|l| l.starts_with("limit used battle=1 actor=0") && l.ends_with("attack=128 target=1 name=A")),
            "{logs:?}"
        );
        assert!(
            logs.iter().any(|l| l.starts_with("limit kept battle=1") && l.contains(&format!("limit={end} was=255")) && l.contains("mirrored=true saving=queued")),
            "{logs:?}"
        );
        assert!(logs.iter().any(|l| l.starts_with("battle started battle=1") && l.contains("limits=[255]") && l.contains("run_difficulty=1 names=[A] companions=0 statuses=[-]")), "{logs:?}");
        assert!(logs.iter().any(|l| l.starts_with("battle ended battle=1 outcome=Victory") && l.contains(&format!("limits=[{end}] companions=0 status_inflicted=0 status_ended=0 status_ticks=0 counters=0"))), "{logs:?}");
        // A win pays as before: the payment is its own job, and the next battle starts from the gauge kept.
        assert_eq!(jobs.iter().filter(|j| j.paid().is_some()).count(), 1);
        assert_eq!(w.request_encounter(a.entity, Some(0), ms(t0, 700_000)), BattleReply::Done);
        match a.battle_msgs().last() {
            Some(ServerMsg::EncounterStart { party, .. }) => assert_eq!(party[0].limit, end),
            other => panic!("{other:?}"),
        }
    }

    /// A counter is an enemy attack inside the events of a COMMAND: the server counts each one, says it as a log line
    /// (`counter battle=.. actor=.. attack=..`) and the `battle ended` line ends with the total. Only enemy attacks count:
    /// a party member's own action, its effects and everything else in the batch say nothing.
    #[test]
    fn an_enemy_attack_inside_a_commands_events_is_a_counter_and_is_counted_and_logged() {
        let t0 = Instant::now();
        let mut w = world_with(Some(settings(SyntheticRules::default())), true);
        let mut a = seat(&mut w, "A", t0);
        assert_eq!(w.request_encounter(a.entity, Some(1), t0), BattleReply::Done);
        let id = seat_battle_id(&a.battle_msgs());
        let foe = ActorId(3);
        let events = [
            BattleEvent::Action { actor: ActorId(0), action: ActionRef::Attack },
            BattleEvent::Damage { actor: ActorId(0), target: foe, amount: 40, critical: false },
            BattleEvent::Action { actor: foe, action: ActionRef::EnemyAttack(286) },
            BattleEvent::Damage { actor: foe, target: ActorId(0), amount: 25, critical: false },
            BattleEvent::Action { actor: foe, action: ActionRef::EnemyAttack(288) },
            BattleEvent::MpSpent { actor: foe, amount: 0 },
        ];
        let battle = w.battle.as_mut().unwrap().battles.get_mut(&id).unwrap();
        assert_eq!(battle.counters, 0);
        let lines = battle.note_counters(id, &events);
        assert_eq!(lines, vec![format!("counter battle={} actor=3 attack=286", id.0), format!("counter battle={} actor=3 attack=288", id.0)]);
        assert_eq!(battle.counters, 2);
        assert!(battle.note_counters(id, &events[..2]).is_empty(), "a batch without an enemy attack says nothing");
        assert_eq!(battle.counters, 2);
        // And what was counted ends the battle's record (a synthetic battle ends with the count it was left at).
        battle.counters = 7;
        let rules = SyntheticRules::default();
        play_out(&mut w, &mut [&mut a], &rules, t0);
        let ended = logs_of(&mut w).into_iter().find(|l| l.starts_with("battle ended battle=1")).expect("an end line");
        assert!(ended.ends_with(" counters=7"), "{ended}");
    }

    /// `--debug-status`: every member starts the battle with those statuses, they travel in the `EncounterStart`, the
    /// engine announces them with its first step, a client's replay of the whole battle agrees with the server's (the
    /// harness panics at the first divergence), and the server logs what the engine said about them.
    #[test]
    fn debug_status_starts_every_member_with_those_statuses_and_the_whole_battle_replays_and_is_logged() {
        use fmv_ff7_battle::status::{Status, StatusSet};
        let t0 = Instant::now();
        let rules = SyntheticRules::default();
        let mut s = settings(SyntheticRules::default());
        let given: StatusSet = [Status::Silence, Status::Haste, Status::Regen].into_iter().collect();
        s.debug_status = given;
        let mut w = world_with(Some(s), true);
        let mut a = seat(&mut w, "A", t0);
        assert_eq!(w.request_encounter(a.entity, Some(1), t0), BattleReply::Done);
        assert_eq!(roster_of(&mut a).iter().map(|m| m.status).collect::<Vec<_>>(), vec![given]);
        let started = logs_of(&mut w).into_iter().find(|l| l.starts_with("battle started")).expect("a start line");
        assert!(started.ends_with(" statuses=[Silence+Haste+Regen]"), "{started}");
        play_out(&mut w, &mut [&mut a], &rules, t0);
        // The first message of events announces them, in bit order, for the one member.
        let first = a
            .battle_msgs()
            .iter()
            .find_map(|m| match m {
                ServerMsg::BattleEvent { events, .. } => Some(events.clone()),
                _ => None,
            })
            .expect("events");
        let me = ActorId(0);
        assert_eq!(
            first[..3],
            [
                BattleEvent::StatusInflicted { target: me, status: Status::Silence },
                BattleEvent::StatusInflicted { target: me, status: Status::Haste },
                BattleEvent::StatusInflicted { target: me, status: Status::Regen },
            ]
        );
        let logs = logs_of(&mut w);
        for status in ["Silence", "Haste", "Regen"] {
            let line = format!("status inflicted battle=1 actor=0 status={status}");
            assert!(logs.contains(&line), "{line} in {logs:?}");
        }
        let ended = logs.iter().find(|l| l.starts_with("battle ended battle=1")).expect("an end line");
        assert!(ended.contains(" status_inflicted=3 status_ended=") && ended.contains(" status_ticks="), "{ended}");
        // Nothing is stored for it, and a server without the flag starts the same battle clean.
        let mut w = world_with(Some(settings(SyntheticRules::default())), true);
        let mut a = seat(&mut w, "A", t0);
        assert_eq!(w.request_encounter(a.entity, Some(1), t0), BattleReply::Done);
        assert_eq!(roster_of(&mut a).iter().map(|m| m.status).collect::<Vec<_>>(), vec![StatusSet::EMPTY]);
        assert!(logs_of(&mut w).iter().any(|l| l.starts_with("battle started") && l.ends_with(" statuses=[-]")));
    }

    /// The engine's refusals reach the player under their own names: a silenced member cannot cast (and its turn is still
    /// there), a sleeper cannot act until it wakes; and a player that cannot take a turn holds nobody up.
    #[test]
    fn a_silenced_member_cannot_cast_a_sleeper_cannot_act_and_the_server_says_which() {
        use fmv_ff7_battle::status::Status;
        let t0 = Instant::now();
        let hit = Command::Attack { target: ActorId(1) };
        // Silence: Magic is refused, the Attack is not, and a refusal spends no turn.
        let mut s = settings(SyntheticRules::default());
        s.debug_status = [Status::Silence].into_iter().collect();
        let mut w = world_with(Some(s), true);
        let mut a = seat(&mut w, "A", t0);
        assert_eq!(w.request_encounter(a.entity, Some(3), t0), BattleReply::Done);
        a.drain();
        let id = seat_battle_id(&a.log);
        let cast = Command::Magic { spell: 27, target: Target::One(ActorId(1)) };
        assert_eq!(w.battle_command(a.entity, id, ActorId(0), cast, t0), refused(Some(id), BattleRejection::Silenced));
        assert!(w.battle.as_ref().unwrap().battles.get(&id).unwrap().state.ready_actors().contains(&ActorId(0)), "a refusal spends no turn");
        assert_eq!(w.battle_command(a.entity, id, ActorId(0), hit, t0), BattleReply::Done);

        // Sleep: nothing at all is accepted while it lasts, and the owner of a sleeper is not waited for.
        let mut s = settings(SyntheticRules::default());
        s.debug_status = [Status::Sleep].into_iter().collect();
        let mut w = world_with(Some(s), true);
        let mut a = seat(&mut w, "A", t0);
        assert_eq!(w.request_encounter(a.entity, Some(3), t0), BattleReply::Done);
        a.drain();
        let id = seat_battle_id(&a.log);
        for command in [hit, Command::Defend, Command::Magic { spell: 27, target: Target::One(ActorId(1)) }] {
            assert_eq!(w.battle_command(a.entity, id, ActorId(0), command, t0), refused(Some(id), BattleRejection::CannotAct), "{command:?}");
        }
        let battle = w.battle.as_ref().unwrap().battles.get(&id).unwrap();
        assert!(battle.state.ready_actors().is_empty() && battle.player_turns().is_empty(), "it holds a full gauge and owes no turn");
        // Awake again (a blow, or the end of its 30 units: well within a second and a half): the same command is taken.
        w.advance_battles(ms(t0, 1_500));
        assert_eq!(w.battle_command(a.entity, id, ActorId(0), hit, ms(t0, 1_500)), BattleReply::Done);
        let logs = logs_of(&mut w);
        assert!(logs.contains(&"status ended battle=1 actor=0 status=Sleep".to_string()), "{logs:?}");
        // A battle with no status: the same player does owe a turn.
        let mut w = world_with(Some(settings(SyntheticRules::default())), true);
        let a = seat(&mut w, "A", t0);
        assert_eq!(w.request_encounter(a.entity, Some(3), t0), BattleReply::Done);
        let battle = w.battle.as_ref().unwrap().battles.values().next().unwrap();
        assert_eq!(battle.player_turns().len(), 1);
    }

    /// The canon companions and the status: a companion that is asleep takes no turn (the server never asks the engine for
    /// one, and the engine is never asked to refuse), and it takes it as soon as it wakes.
    #[test]
    fn a_sleeping_companion_takes_no_turn_until_it_wakes_and_nothing_is_refused() {
        use fmv_ff7_battle::status::Status;
        let t0 = Instant::now();
        let mut s = settings_unpaced(SyntheticRules::default());
        s.debug_status = [Status::Sleep].into_iter().collect();
        let mut w = world_with_party(s, cloud_and_barret);
        let mut a = seat_as(&mut w, "Aria", t0, ProgressSeed::of(2, None));
        a.drain();
        assert_eq!(w.request_encounter(a.entity, Some(3), t0), BattleReply::Done);
        assert_eq!(roster_of(&mut a).iter().map(|m| m.status.len()).collect::<Vec<_>>(), [1, 1, 1], "everybody starts asleep");
        // A tenth of a second of battle (the enemy has not acted, and Sleep lasts 0.9 s): nobody acted, nothing was refused.
        w.advance_battles(ms(t0, 100));
        let logs = logs_of(&mut w);
        assert!(!logs.iter().any(|l| l.starts_with("companion acted") || l.contains("was refused by the engine")), "{logs:?}");
        assert_eq!(logs.iter().filter(|l| l.starts_with("status inflicted battle=1")).count(), 3, "one Sleep each: {logs:?}");
        // Two seconds on, the sleepers have woken (a blow, or the end of their 30 units) and the companions have taken the
        // turns they held: still nothing was refused.
        w.advance_battles(ms(t0, 2_000));
        let logs = logs_of(&mut w);
        assert!(logs.iter().any(|l| l.starts_with("status ended battle=1 ") && l.ends_with(" status=Sleep")), "{logs:?}");
        assert!(logs.iter().any(|l| l.starts_with("companion acted battle=1")), "they act once awake: {logs:?}");
        assert!(!logs.iter().any(|l| l.contains("was refused by the engine")), "{logs:?}");
    }

    /// `--debug-limit-full`: the battle STARTS with full gauges, whatever the accounts have, and NO gauge is ever
    /// stored under it: not a full one nobody spent, and not a spent one either (it started from a faked gauge, so
    /// it is not the account's own). The accounts keep what they really have.
    #[test]
    fn debug_limit_full_only_changes_the_gauge_a_battle_starts_with_and_never_stores_a_gauge() {
        let rules = player_limit_rules();
        let t0 = Instant::now();
        let mut s = settings(rules);
        s.debug_limit_full = true;
        let (mut w, mut rx) = world_with_saver(s);
        // Two accounts with gauges of their own: an empty one, and 100.
        let stored = Progress { character: 0, level: 5, exp: 40, ap: 0, gil: 0, limit: 100 };
        let (mut a, b) = party_of_two_as(&mut w, t0, ProgressSeed::default(), ProgressSeed::of(0, Some(stored)));
        assert_eq!((mirrored_limit(&w, &a), mirrored_limit(&w, &b)), (0, 100));
        let started_with = |seat: &mut Seat| match seat.battle_msgs().last() {
            Some(ServerMsg::EncounterStart { party, .. }) => party.iter().map(|m| m.limit).collect::<Vec<_>>(),
            other => panic!("{other:?}"),
        };

        // Formation 3 is the preemptive one: a sure escape, with every party gauge full at once. Both start
        // with a full limit gauge (the flag fakes it) and, under the flag, NO gauge is ever written: the
        // accounts keep what they really have, and the server says why.
        assert_eq!(w.request_encounter(a.entity, Some(3), t0), BattleReply::Done);
        assert_eq!(started_with(&mut a), vec![255, 255], "full, whatever the accounts have");
        let first = seat_battle_id(&a.log);
        assert_eq!(w.battle_command(a.entity, first, ActorId(0), Command::Escape, t0), BattleReply::Done);
        assert_eq!(w.battle_count(), 0);
        assert!(saved_all(&mut rx).is_empty(), "a faked full gauge that nobody spent is not stored");
        assert_eq!((mirrored_limit(&w, &a), mirrored_limit(&w, &b)), (0, 100), "the accounts keep their own gauges");
        let logs = logs_of(&mut w);
        assert!(logs.iter().any(|l| l.starts_with("battle started battle=1") && l.contains("limits=[255, 255]")), "{logs:?}");
        assert!(logs.iter().any(|l| l == "limit not kept battle=1 reason=debug-limit-full"), "the early return ran: {logs:?}");
        assert!(!logs.iter().any(|l| l.starts_with("limit kept")), "{logs:?}");

        // Next battle: A gives its limit (B does nothing), so A ENDS below the faked start. Even that moved
        // gauge is not written: it started from the faked full one, so it is not A's own.
        assert_eq!(w.request_encounter(a.entity, Some(3), t0), BattleReply::Done);
        assert_eq!(started_with(&mut a), vec![255, 255], "every battle starts them full again");
        let second = seat_battle_id(&a.log);
        assert_eq!(w.battle_command(a.entity, second, ActorId(0), Command::Limit { target: Target::One(ActorId(2)) }, t0), BattleReply::Done);
        play_out(&mut w, &mut [&mut a], &rules, t0);
        let gauges = replayed_gauges(&mut a, &rules);
        assert_ne!(gauges[0], 255, "A's limit emptied its gauge");
        let jobs = saved_all(&mut rx);
        assert!(limits_of(&jobs).is_empty(), "a gauge that started from the faked full one is never stored: {jobs:?}");
        assert_eq!((mirrored_limit(&w, &a), mirrored_limit(&w, &b)), (0, 100), "both accounts keep the gauges they really have");
        let logs = logs_of(&mut w);
        assert!(logs.iter().any(|l| l.starts_with("limit used battle=2 actor=0")), "{logs:?}");
        assert!(logs.iter().any(|l| l == "limit not kept battle=2 reason=debug-limit-full"), "{logs:?}");
        assert!(!logs.iter().any(|l| l.starts_with("limit kept")), "{logs:?}");

        // Without the flag, the same accounts start with their own gauges.
        let (mut w, _rx) = world_with_saver(settings(rules));
        let (mut a, _b) = party_of_two_as(&mut w, t0, ProgressSeed::default(), ProgressSeed::of(0, Some(stored)));
        assert_eq!(w.request_encounter(a.entity, Some(3), t0), BattleReply::Done);
        assert_eq!(started_with(&mut a), vec![0, 100], "the accounts' own gauges");
    }

    /// Magic, Defend and Item are logged when the engine applies them (the server's own record, next to
    /// `limit used` and `escape tried`); a command that is refused is not.
    #[test]
    fn magic_defend_and_item_are_logged_as_they_are_applied_and_a_refused_command_is_not() {
        let rules = player_limit_rules();
        let t0 = Instant::now();
        let mut w = world_with(Some(settings(rules)), true);
        let mut a = seat(&mut w, "A", t0);
        assert_eq!(w.request_encounter(a.entity, Some(3), t0), BattleReply::Done);
        a.battle_msgs();
        let id = seat_battle_id(&a.log);
        // A spell the character does not know is refused, and says nothing in the log.
        assert_eq!(
            w.battle_command(a.entity, id, ActorId(0), Command::Magic { spell: 99, target: Target::One(ActorId(0)) }, t0),
            refused(Some(id), BattleRejection::UnknownSpell)
        );
        let mut turn = 0;
        play_with(&mut w, &mut [&mut a], &rules, t0, &mut |state, me| {
            turn += 1;
            match turn {
                1 => Command::Defend,
                2 => Command::Magic { spell: 0, target: Target::One(me) },
                3 => Command::Item { item: 0, target: Target::One(me) },
                _ => Command::Attack { target: state.enemy_ids().into_iter().find(|e| state.actor(*e).is_some_and(|c| c.is_alive())).unwrap() },
            }
        });
        let logs = logs_of(&mut w);
        let account = a.account.0;
        let has = |line: String| logs.contains(&line);
        assert!(has(format!("defend used battle=1 actor=0 account={account} character=0 name=A")), "{logs:?}");
        assert!(has(format!("magic used battle=1 actor=0 account={account} character=0 spell=0 target=0 name=A")), "{logs:?}");
        assert!(logs.iter().any(|l| l.starts_with("item used battle=1 ") && l.contains("item=0 left=2")), "a Potion of three: {logs:?}");
        assert_eq!(logs.iter().filter(|l| l.starts_with("magic used")).count(), 1, "the refused spell is not logged: {logs:?}");
        assert!(!logs.iter().any(|l| l.contains("spell=99")), "{logs:?}");
    }

    #[test]
    fn a_gauge_that_did_not_move_is_not_saved_and_a_stored_one_above_the_cap_is_brought_inside() {
        let rules = player_limit_rules();
        let t0 = Instant::now();
        let (mut w, mut rx) = world_with_saver(settings(rules));
        // A hostile or hand-edited record: the join brings it inside the cap, and so does the battle.
        let wild = Progress { character: 0, level: 5, exp: 40, ap: 0, gil: 0, limit: u16::MAX };
        let mut a = seat_as(&mut w, "A", t0, ProgressSeed::of(0, Some(wild)));
        assert_eq!(mirrored_limit(&w, &a), 255);
        assert_eq!(w.request_encounter(a.entity, Some(3), t0), BattleReply::Done);
        match &a.battle_msgs()[..] {
            [ServerMsg::EncounterStart { party, .. }] => assert_eq!(party[0].limit, 255),
            other => panic!("{other:?}"),
        }
        let id = seat_battle_id(&a.log);
        // Escape at once (the preemptive formation: a sure escape) before a single blow: nothing moved.
        assert_eq!(w.battle_command(a.entity, id, ActorId(0), Command::Escape, t0), BattleReply::Done);
        assert_eq!(w.battle_count(), 0);
        assert!(saved_all(&mut rx).is_empty(), "the gauge ended as it started: nothing to write");
        assert!(!logs_of(&mut w).iter().any(|l| l.starts_with("limit kept")));
    }

    /// Players have no limit break (protocol v9), so the engine reports an empty gauge for them whatever the account
    /// holds: the server must never write that 0 over a stored gauge ("a number moved"). Played to the end under the
    /// real rule (the synthetic default), through a win and an escape, the stored gauge, level, experience and the
    /// character's slot are exactly what they were, no limit is queued for the saver and nothing says it kept one.
    #[test]
    fn a_players_stored_gauge_is_never_overwritten_by_the_empty_one_a_limitless_character_has() {
        let rules = SyntheticRules::default();
        let t0 = Instant::now();
        let (mut w, mut rx) = world_with_saver(settings(rules));
        // A v8-written record: level 5, 40 experience (the synthetic start), a gauge of 120.
        let stored = Progress { character: 2, level: 5, exp: 40, ap: 0, gil: 0, limit: 120 };
        let mut a = seat_as(&mut w, "A", t0, ProgressSeed::of(2, Some(stored)));
        assert_eq!(mirrored_limit(&w, &a), 120);
        // Formation 3 is the preemptive one (a sure escape): the first battle is an escape at once.
        assert_eq!(w.request_encounter(a.entity, Some(3), t0), BattleReply::Done);
        match &a.battle_msgs()[..] {
            [ServerMsg::EncounterStart { party, .. }] => {
                assert_eq!((party[0].character, party[0].level), (2, 5));
                assert_eq!(party[0].limit, 120, "the account's gauge is what the roster carries; the engine ignores it");
            }
            other => panic!("{other:?}"),
        }
        let id = seat_battle_id(&a.log);
        assert!(w.battle.as_ref().unwrap().battles.get(&id).unwrap().state.limit_for(ActorId(0)).is_none(), "a player has no limit break");
        assert_eq!(w.battle_command(a.entity, id, ActorId(0), Command::Limit { target: Target::One(ActorId(1)) }, t0), refused(Some(id), BattleRejection::NoLimit));
        assert_eq!(w.battle_command(a.entity, id, ActorId(0), Command::Escape, t0), BattleReply::Done);
        assert_eq!(w.battle_count(), 0);
        // A second battle, a win this time (the synthetic two-dummy formation pays experience).
        let later = ms(t0, 700_000);
        assert_eq!(w.request_encounter(a.entity, Some(1), later), BattleReply::Done);
        play_out(&mut w, &mut [&mut a], &rules, later);
        let jobs = saved_all(&mut rx);
        assert!(limits_of(&jobs).is_empty(), "no limit gauge is queued for a character that has none: {jobs:?}");
        assert!(jobs.iter().any(|j| j.paid().is_some()), "the win itself was paid and saved");
        assert_eq!(mirrored_limit(&w, &a), 120, "the mirror keeps the gauge too");
        let progress = w.players.get(&a.entity).and_then(|p| p.progress).expect("a mirror");
        assert_eq!(progress.character, 2, "the slot did not move");
        assert!(progress.exp > 40, "the win was paid on top of what the account had");
        let logs = logs_of(&mut w);
        assert!(!logs.iter().any(|l| l.starts_with("limit kept") || l.starts_with("limit used")), "{logs:?}");
    }

    #[test]
    fn an_escape_ends_the_battle_for_the_whole_party_pays_nothing_and_keeps_the_gauges_the_blows_filled() {
        let rules = player_limit_rules();
        let t0 = Instant::now();
        let (mut w, mut rx) = world_with_saver(settings(rules));
        let (mut a, mut b) = party_of_two(&mut w, t0);
        assert_eq!(w.request_encounter(a.entity, Some(1), t0), BattleReply::Done);
        // A attacks until a blow has filled its gauge, then runs until it gets away; B never acts.
        play_with(&mut w, &mut [&mut a], &rules, t0, &mut |state, me| {
            if state.limit_gauge(me) > 0 {
                Command::Escape
            } else {
                Command::Attack { target: state.enemy_ids().into_iter().find(|e| state.actor(*e).is_some_and(|c| c.is_alive())).unwrap() }
            }
        });
        for seat in [&mut a, &mut b] {
            let ends: Vec<_> = seat.battle_msgs().into_iter().filter_map(|m| match m {
                ServerMsg::BattleEnd { outcome, rewards, .. } => Some((outcome, rewards)),
                _ => None,
            }).collect();
            assert_eq!(ends, vec![(Outcome::Escaped, vec![])], "everyone hears it, and nothing is paid");
            assert!(!w.in_battle(seat.entity), "back in the field");
        }
        assert_eq!(w.battle_count(), 0);
        // Nothing was paid or used: the queue holds gauges only.
        let jobs = saved_all(&mut rx);
        assert!(jobs.iter().all(|j| matches!(j.change, crate::saver::SavedChange::Limit(_))), "{jobs:?}");
        assert_eq!(progress_msgs(&mut a).len(), 1, "only the join message: an escape tells nobody a new level or total");
        // The gauges the blows filled are the ones a replay computes, kept for every member, A's included.
        let gauges = replayed_gauges(&mut a, &rules);
        assert!(gauges[0] > 0, "the blows that made A run filled its gauge");
        let mut kept = limits_of(&jobs);
        kept.sort();
        let mut want: Vec<(AccountId, u8, u16)> =
            [(a.account, gauges[0]), (b.account, gauges[1])].into_iter().filter(|(_, g)| *g != 0).map(|(account, g)| (account, 0, g)).collect();
        want.sort();
        assert_eq!(kept, want);
        assert_eq!(mirrored_limit(&w, &a), gauges[0]);
        assert_eq!(mirrored_limit(&w, &b), gauges[1]);
        let logs = logs_of(&mut w);
        assert!(logs.iter().any(|l| l.starts_with("battle ended battle=1 outcome=Escaped")), "{logs:?}");
        let tries: Vec<&String> = logs.iter().filter(|l| l.starts_with("escape tried battle=1 actor=0")).collect();
        assert!(tries.last().is_some_and(|l| l.contains("result=escaped")), "{tries:?}");
        assert!(tries.iter().rev().skip(1).all(|l| l.contains("result=failed")), "only the last attempt got away: {tries:?}");
        // No reward line: an escape pays nothing, not even a payment of nothing.
        assert!(!logs.iter().any(|l| l.starts_with("battle reward")), "{logs:?}");
    }

    #[test]
    fn a_limit_or_an_escape_the_engine_refuses_is_refused_with_its_own_reason_and_changes_nothing() {
        let rules = player_limit_rules();
        let t0 = Instant::now();
        let (mut w, mut rx) = world_with_saver(settings(rules));
        // Formation 2 (the overwhelming enemy) cannot be run from; the stored gauge is half full.
        let half = Progress { character: 0, level: 5, exp: 40, ap: 0, gil: 0, limit: 100 };
        let a = seat_as(&mut w, "A", t0, ProgressSeed::of(0, Some(half)));
        assert_eq!(w.request_encounter(a.entity, Some(2), t0), BattleReply::Done);
        let id = BattleId(1);
        // Until its turn comes nothing can be given; then the reasons are the engine's, in its order.
        let mut reason = None;
        for step in 1..=200u64 {
            let now = ms(t0, step * 10);
            w.advance_battles(now);
            match w.battle_command(a.entity, id, ActorId(0), Command::Escape, now) {
                BattleReply::Refused { reason: BattleRejection::GaugeNotFull, .. } => continue,
                BattleReply::Refused { reason: r, .. } => {
                    reason = Some((r, now));
                    break;
                }
                other => panic!("{other:?}"),
            }
        }
        let (r, now) = reason.expect("the character's turn comes");
        assert_eq!(r, BattleRejection::CannotEscape);
        assert_eq!(
            w.battle_command(a.entity, id, ActorId(0), Command::Limit { target: Target::One(ActorId(1)) }, now),
            refused(Some(id), BattleRejection::LimitNotReady),
            "a gauge of 100 is not a full one"
        );
        assert_eq!(w.battle_count(), 1, "refusals end nothing");
        assert!(saved_all(&mut rx).is_empty());
        assert!(!logs_of(&mut w).iter().any(|l| l.starts_with("escape tried") || l.starts_with("limit used")));
    }

    #[test]
    fn a_healing_limit_aims_at_the_whole_party_and_a_damaging_one_at_one_enemy() {
        let rules = player_limit_rules();
        let t0 = Instant::now();
        let (mut w, _rx) = world_with_saver(settings(rules));
        // Character 3 (Aerith) heals; Cloud strikes. Both start with a full gauge in a preemptive battle.
        let full = |character| ProgressSeed::of(character, Some(Progress { character, level: 5, exp: 40, ap: 0, gil: 0, limit: 255 }));
        let (mut a, mut b) = party_of_two_as(&mut w, t0, full(0), full(3));
        assert_eq!(w.request_encounter(a.entity, Some(3), t0), BattleReply::Done);
        a.drain();
        b.drain();
        let id = seat_battle_id(&a.log);
        let (cloud, aerith, enemy) = (ActorId(0), ActorId(1), ActorId(2));
        let refuse = |reason| refused(Some(id), reason);
        // Cloud's strike needs one enemy (not an ally, not a whole side); Aerith's wind is the whole party and
        // nothing smaller or other (Healing Wind's record says "All Al (NS)", protocol v10).
        let strike = |target| Command::Limit { target };
        for target in [Target::One(aerith), Target::AllAllies, Target::AllEnemies] {
            assert_eq!(w.battle_command(a.entity, id, cloud, strike(target), t0), refuse(BattleRejection::InvalidTarget), "{target:?}");
        }
        for target in [Target::One(enemy), Target::One(cloud), Target::One(aerith), Target::AllEnemies] {
            assert_eq!(w.battle_command(b.entity, id, aerith, strike(target), t0), refuse(BattleRejection::InvalidTarget), "{target:?}");
        }
        assert_eq!(w.battle_command(b.entity, id, aerith, strike(Target::AllAllies), t0), BattleReply::Done);
        assert_eq!(w.battle_command(a.entity, id, cloud, strike(Target::One(enemy)), t0), BattleReply::Done);
        // The server's own record of what it applied says whom: a number for one actor, a side for a whole one.
        let lines = logs_of(&mut w);
        assert!(lines.iter().any(|l| l.starts_with("limit used") && logged(l, "character") == Some("3") && logged(l, "target") == Some("all-allies")), "{lines:?}");
        assert!(lines.iter().any(|l| l.starts_with("limit used") && logged(l, "character") == Some("0") && logged(l, "target") == Some("2")), "{lines:?}");
        // A member cannot give another member's limit.
        assert_eq!(w.battle_command(a.entity, id, aerith, Command::Escape, t0), refuse(BattleRejection::NotYourActor));
    }

    /// With the canon pair in front the party side of the engine is [Cloud, Barret, players..] (protocol v9): a player can
    /// aim a Cure or a Potion at a companion (one target), and a cast on the whole party reaches the companions and the
    /// players alike (whole-side targeting, protocol v10: `AllAllies` is the commanding actor's own side, all of it). A
    /// player still commands only its own actor, and the server's record says whom.
    #[test]
    fn a_player_can_heal_a_companion_and_a_whole_party_cast_reaches_the_companions_and_the_players() {
        let t0 = Instant::now();
        // A dummy that outlasts the test, so that the canon pair's attacks do not end the battle. (The companions' pacing
        // is off: this is about whom a command can aim at, not about when a companion acts.)
        let rules = SyntheticRules { enemy_hp: 100_000, ..SyntheticRules::default() };
        let mut w = world_with_party(settings_unpaced(rules), cloud_and_barret);
        let (mut a, mut b) = (seat(&mut w, "Aria", t0), seat(&mut w, "Brom", t0));
        party_up(&mut w, &a, &b, t0);
        a.drain();
        b.drain();
        // Formation 3 starts with every gauge full: Cloud (0), Barret (1), Aria (2), Brom (3), the dummy (4).
        assert_eq!(w.request_encounter(a.entity, Some(3), t0), BattleReply::Done);
        let id = BattleId(1);
        let (cloud, barret, aria, brom) = (ActorId(0), ActorId(1), ActorId(2), ActorId(3));
        // One target each: Aria cures the canon Cloud, Brom gives Barret a Potion (a new account carries three).
        assert_eq!(w.battle_command(a.entity, id, aria, Command::Magic { spell: 0, target: Target::One(cloud) }, t0), BattleReply::Done);
        assert_eq!(w.battle_command(b.entity, id, brom, Command::Item { item: 0, target: Target::One(barret) }, t0), BattleReply::Done);
        // A player commands its own actor only, whoever its spell is aimed at.
        assert_eq!(w.battle_command(a.entity, id, brom, Command::Defend, t0), refused(Some(id), BattleRejection::NotYourActor));
        assert_eq!(w.battle_command(a.entity, id, cloud, Command::Magic { spell: 0, target: Target::AllAllies }, t0), refused(Some(id), BattleRejection::NotYourActor));
        // The whole party, on Aria's next turn (her gauge has to fill again): one Cure, one All use, everybody healed.
        let whole = Command::Magic { spell: 0, target: Target::AllAllies };
        let mut applied = false;
        for step in 1..=400u64 {
            let now = ms(t0, step * 10);
            w.advance_battles(now);
            match w.battle_command(a.entity, id, aria, whole, now) {
                BattleReply::Refused { reason: BattleRejection::GaugeNotFull, .. } => continue,
                other => {
                    assert_eq!(other, BattleReply::Done);
                    applied = true;
                    break;
                }
            }
        }
        assert!(applied, "Aria's turn came");
        // Brom (who has not used his All) sees it as one command from Aria and a heal for each of the four members.
        let healed: Vec<ActorId> = b
            .battle_msgs()
            .iter()
            .filter_map(|m| match m {
                ServerMsg::BattleEvent { events, command: Some(AppliedCommand { actor, command }), .. } if *actor == aria && *command == whole => Some(events.clone()),
                _ => None,
            })
            .flatten()
            .filter_map(|e| if let BattleEvent::Heal { target, .. } = e { Some(target) } else { None })
            .collect();
        assert_eq!(healed, [cloud, barret, aria, brom], "both companions and both players, in actor order, from one Cure");
        // The server's own record says whom: an actor's number for one target, the side for the whole party.
        let lines = logs_of(&mut w);
        let casts: Vec<(Option<&str>, Option<&str>)> =
            lines.iter().filter(|l| l.starts_with("magic used")).map(|l| (logged(l, "actor"), logged(l, "target"))).collect();
        assert_eq!(casts, [(Some("2"), Some("0")), (Some("2"), Some("all-allies"))], "{lines:?}");
        assert!(lines.iter().any(|l| l.starts_with("item used") && logged(l, "item") == Some("0")), "{lines:?}");
        // The whole-party cast spent Aria's one All use: another is refused, and a one-target cast is not.
        let mut refused_again = None;
        for step in 1..=400u64 {
            let now = ms(t0, 4_000 + step * 10);
            w.advance_battles(now);
            match w.battle_command(a.entity, id, aria, whole, now) {
                BattleReply::Refused { reason: BattleRejection::GaugeNotFull, .. } => continue,
                other => {
                    refused_again = Some(other);
                    break;
                }
            }
        }
        assert_eq!(refused_again, Some(refused(Some(id), BattleRejection::InvalidTarget)), "no All use left");
    }

    #[test]
    fn a_defeat_pays_nothing_and_changes_nothing() {
        let rules = player_limit_rules();
        let t0 = Instant::now();
        let (mut w, mut rx) = world_with_saver(settings(rules));
        let (mut a, mut b) = party_of_two(&mut w, t0);
        let before = (progress_msgs(&mut a), progress_msgs(&mut b));
        assert_eq!(w.request_encounter(a.entity, Some(2), t0), BattleReply::Done);
        play_out(&mut w, &mut [&mut a, &mut b], &rules, t0);
        assert_eq!((progress_msgs(&mut a), progress_msgs(&mut b)), before, "no progress message after a defeat");
        assert!(saved(&mut rx).is_empty(), "nothing to save");
        let mirror: Vec<Progress> = w.players.values().filter_map(|p| p.progress).collect();
        assert_eq!(mirror, vec![Progress { character: 0, level: 5, exp: 40, ap: 0, gil: 0, limit: 0 }; 2]);
    }

    /// A member that left mid-battle still fought: what it earned reaches the store (the saver is
    /// the only thing that can reach an offline account) but nobody is told.
    #[test]
    fn a_member_who_left_is_paid_in_the_store_and_told_nothing() {
        let rules = player_limit_rules();
        let t0 = Instant::now();
        let (mut w, mut rx) = world_with_saver(settings(rules));
        let (mut a, b) = party_of_two(&mut w, t0);
        assert_eq!(w.request_encounter(a.entity, Some(0), t0), BattleReply::Done);
        let (b_account, b_entity) = (b.account, b.entity);
        assert!(w.leave(b_entity));
        drop(b);
        play_out(&mut w, &mut [&mut a], &rules, t0);
        let jobs = saved(&mut rx);
        let accounts: Vec<AccountId> = jobs.iter().map(|j| j.account).collect();
        assert!(accounts.contains(&b_account), "the account that left is still paid: {jobs:?}");
        assert!(accounts.contains(&a.account), "{jobs:?}");
        // The one that stayed was told about its own payment only.
        assert_eq!(progress_msgs(&mut a).len(), 2, "the join message and its own payment");
    }

    /// A payment of nothing (a member that was dead when the others won) changes nothing and is not
    /// queued, but the member that is online is still told where it stands.
    #[test]
    fn a_member_that_was_dead_at_the_end_is_not_paid_a_thing_but_is_still_told_where_it_stands() {
        let t0 = Instant::now();
        let (mut w, mut rx) = world_with_saver(settings(SyntheticRules::default()));
        let (mut a, b) = party_of_two(&mut w, t0);
        a.drain();
        let members: Vec<Member> = [(&a, 0u8), (&b, 1u8)]
            .iter()
            .map(|(seat, character)| Member {
                info: BattleMember::player(seat.entity, seat.account, "Tester", *character, 5, 0),
                connected: true,
            })
            .collect();
        let rewards = [
            BattleReward::new(a.account, 24, 40, 4),
            BattleReward::new(b.account, 0, 0, 0),
        ];
        let told = w.pay_members(BattleId(1), &members, &rewards);
        assert_eq!(told.iter().map(|(entity, _)| *entity).collect::<Vec<_>>(), vec![a.entity, b.entity], "everybody online hears where it stands, in roster order");
        let said = |i: usize| match &*told[i].1 {
            ServerMsg::CharacterProgress { level, exp, gil, ap, leveled_up, .. } => (*level, *exp, *gil, *ap, *leveled_up),
            other => panic!("{other:?}"),
        };
        assert_eq!(said(0), (6, 64, 40, 4, true), "the living member was paid 24 and crossed level 6 (60)");
        assert_eq!(said(1), (5, 40, 0, 0, false), "the dead one is told what it has, unchanged");
        // Only the payment that was made is queued for the store.
        let jobs = saved(&mut rx);
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].account, a.account);
        // B's mirror is exactly what it was.
        assert_eq!(w.players.get(&b.entity).unwrap().progress, Some(Progress { character: 0, level: 5, exp: 40, ap: 0, gil: 0, limit: 0 }));
    }

    // ---------------------------------------------------------------------------------------------
    // --debug-bonus-exp / --debug-bonus-limit: made-up numbers through the ordinary road
    // ---------------------------------------------------------------------------------------------

    /// The experience bonus is paid to a member that fought on and only to it, on top of what the battle gave it, and it
    /// goes down the ordinary road: the mirror (the game's own level-up rule), the saver and the log line, which says what
    /// was really paid and that it was a debug bonus. A member with nothing to be paid gets no bonus either.
    #[test]
    fn the_debug_experience_bonus_is_paid_on_top_to_the_members_that_fought_on_through_the_ordinary_road() {
        let t0 = Instant::now();
        let mut s = settings(SyntheticRules::default());
        s.debug_bonus_exp = 300;
        let (mut w, mut rx) = world_with_saver(s);
        let (mut a, b) = party_of_two(&mut w, t0);
        a.drain();
        let members: Vec<Member> = [(&a, 0u8), (&b, 1u8)]
            .iter()
            .map(|(seat, character)| Member {
                info: BattleMember::player(seat.entity, seat.account, "Tester", *character, 5, 0),
                connected: true,
            })
            .collect();
        let rewards = [BattleReward::new(a.account, 24, 40, 4), BattleReward::new(b.account, 0, 0, 0)];
        let told = w.pay_members(BattleId(1), &members, &rewards);
        let said = |i: usize| match &*told[i].1 {
            ServerMsg::CharacterProgress { level, exp, gil, ap, leveled_up, .. } => (*level, *exp, *gil, *ap, *leveled_up),
            other => panic!("{other:?}"),
        };
        // 40 + 24 + 300 = 364 is exactly level 14's threshold (2 * 14 * 13): many levels, by the game's own rule.
        assert_eq!(said(0), (14, 364, 40, 4, true), "paid 24 and the 300 bonus; gil and AP are not touched");
        assert_eq!(said(1), (5, 40, 0, 0, false), "nothing earned, so no bonus either");
        let jobs = saved(&mut rx);
        assert_eq!(jobs, vec![crate::saver::SaveJob::reward(a.account, 0, BattleReward::new(a.account, 324, 40, 4))], "what is saved is what was paid");
        let logs = logs_of(&mut w);
        assert!(
            logs.iter().any(|l| l.starts_with(&format!("battle reward battle=1 account={} exp=324 gil=40 ap=4 ", a.account.0)) && l.contains(" debug_bonus_exp=300")),
            "{logs:?}"
        );
        assert_eq!(
            logs.iter().filter(|l| l.starts_with("battle reward") && l.contains("debug_bonus")).count(),
            1,
            "only the paid member's line says so, the unpaid member's does not: {logs:?}"
        );
    }

    /// Through a real won battle: the `BattleEnd` the clients verify by replay (the harness replays it and checks the
    /// rewards) still carries what the battle itself earned, and what the player is told and what is saved is what was paid.
    #[test]
    fn the_debug_experience_bonus_leaves_the_battle_end_the_clients_verify_as_it_was() {
        let rules = SyntheticRules::default();
        let t0 = Instant::now();
        let mut s = settings(rules);
        s.debug_bonus_exp = 300;
        let (mut w, mut rx) = world_with_saver(s);
        let mut a = seat(&mut w, "A", t0); // Cloud, level 5, 40 experience
        // Formation 0 is one dummy: 12 experience, 20 gil, 2 AP.
        assert_eq!(w.request_encounter(a.entity, Some(0), t0), BattleReply::Done);
        play_out(&mut w, &mut [&mut a], &rules, t0); // (it replays the battle and checks the BattleEnd against it)
        let ends: Vec<Vec<BattleReward>> = a
            .log
            .iter()
            .filter_map(|m| match m {
                ServerMsg::BattleEnd { rewards, .. } => Some(rewards.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(ends, vec![vec![BattleReward::new(a.account, 12, 20, 2)]], "the clients verify what the battle earned");
        // 40 + 12 + 300 = 352: level 13 (312), the next one at 364.
        assert_eq!(*progress_msgs(&mut a).last().unwrap(), (0, 13, 352, 12, 20, 2, true));
        assert_eq!(saved(&mut rx), vec![crate::saver::SaveJob::reward(a.account, 0, BattleReward::new(a.account, 312, 20, 2))]);
    }

    /// The limit bonus: the gauge kept is the one the battle left plus the bonus, up to the game's cap, saved and mirrored
    /// like any other, logged with the bonus, and the next battle starts with it.
    #[test]
    fn the_debug_limit_bonus_is_kept_saved_mirrored_capped_and_carried_into_the_next_battle() {
        let rules = player_limit_rules();
        let t0 = Instant::now();
        let mut s = settings(rules);
        s.debug_bonus_limit = 100;
        let (mut w, mut rx) = world_with_saver(s);
        let mut a = seat(&mut w, "A", t0); // an empty gauge
        let started_with = |seat: &mut Seat| match seat.battle_msgs().last() {
            Some(ServerMsg::EncounterStart { party, .. }) => party[0].limit,
            other => panic!("{other:?}"),
        };
        // Three battles in a row. What a battle leaves is read from a replay of what the player was told (the oracle the
        // other limit tests use), so the arithmetic is the engine's, not a number copied here: the kept gauge is that plus
        // 100, up to the cap; it is what is saved and mirrored, and what the next battle starts with.
        let mut start = 0u16;
        let mut kept_gauges = Vec::new();
        for battle in 1..=3u64 {
            assert_eq!(w.request_encounter(a.entity, Some(0), t0), BattleReply::Done);
            assert_eq!(started_with(&mut a), start, "battle {battle} starts with the gauge the last one kept");
            play_out(&mut w, &mut [&mut a], &rules, t0);
            let left = replayed_gauges(&mut a, &rules)[0];
            let kept = (left + 100).min(255);
            let jobs = limits_of(&saved_all(&mut rx));
            if kept == start {
                assert!(jobs.is_empty(), "a gauge that did not move writes nothing: {jobs:?}");
            } else {
                assert_eq!(jobs, vec![(a.account, 0, kept)], "battle {battle}: left {left}, kept {kept}");
            }
            assert_eq!(mirrored_limit(&w, &a), kept);
            let logs = logs_of(&mut w);
            if kept != start {
                let line = format!("limit kept battle={battle} account={} character=0 limit={kept} was={start} mirrored=true saving=queued debug_bonus_limit=100", a.account.0);
                assert!(logs.contains(&line), "{line}\n{logs:?}");
            }
            kept_gauges.push(kept);
            start = kept;
        }
        assert!(kept_gauges[0] > 100, "the first battle kept the bonus on top of what it left: {kept_gauges:?}");
        assert!(kept_gauges.contains(&255) && kept_gauges.iter().all(|g| *g <= 255), "the cap is reached and holds: {kept_gauges:?}");
    }

    /// Under the real rule (players have no limit break, protocol v9) there is no gauge for the debug bonus to be added
    /// to: a player's stored gauge is left exactly as it is, bonus or not, and nothing is queued, mirrored or logged as
    /// kept. (A debug run can therefore never turn a player's gauge into a made-up number either.)
    #[test]
    fn the_debug_limit_bonus_never_touches_the_stored_gauge_of_a_player_who_has_no_limit_break() {
        let rules = SyntheticRules::default();
        let t0 = Instant::now();
        let mut s = settings(rules);
        s.debug_bonus_limit = 100;
        let (mut w, mut rx) = world_with_saver(s);
        let stored = Progress { character: 0, level: 5, exp: 40, ap: 0, gil: 0, limit: 200 };
        let mut a = seat_as(&mut w, "A", t0, ProgressSeed::of(0, Some(stored)));
        assert_eq!(w.request_encounter(a.entity, Some(0), t0), BattleReply::Done);
        play_out(&mut w, &mut [&mut a], &rules, t0);
        let jobs = saved_all(&mut rx);
        assert!(limits_of(&jobs).is_empty(), "no gauge is queued for a character that has none: {jobs:?}");
        assert_eq!(mirrored_limit(&w, &a), 200, "the mirror keeps the stored gauge too");
        let logs = logs_of(&mut w);
        assert!(!logs.iter().any(|l| l.starts_with("limit kept")), "{logs:?}");
    }

    /// With the flags off (the default) nothing is added anywhere: the lines do not mention them either.
    #[test]
    fn without_the_debug_bonus_flags_nothing_is_added_and_the_lines_do_not_mention_them() {
        let rules = player_limit_rules();
        let t0 = Instant::now();
        let (mut w, mut rx) = world_with_saver(settings(rules));
        let mut a = seat(&mut w, "A", t0);
        assert_eq!(w.request_encounter(a.entity, Some(0), t0), BattleReply::Done);
        play_out(&mut w, &mut [&mut a], &rules, t0);
        let left = replayed_gauges(&mut a, &rules)[0];
        let jobs = saved_all(&mut rx);
        assert!(jobs.iter().any(|j| *j == crate::saver::SaveJob::reward(a.account, 0, BattleReward::new(a.account, 12, 20, 2))), "{jobs:?}");
        assert_eq!(limits_of(&jobs), if left == 0 { vec![] } else { vec![(a.account, 0, left)] }, "the gauge the battle left, nothing added");
        assert_eq!(mirrored_limit(&w, &a), left);
        let logs = logs_of(&mut w);
        assert!(!logs.iter().any(|l| l.contains("debug_bonus")), "{logs:?}");
    }

    /// Without a saver (the world's unit tests, a server that stores nothing) payments still reach
    /// the mirror and the player.
    #[test]
    fn without_a_saver_the_player_is_still_paid_and_told() {
        let rules = SyntheticRules::default();
        let t0 = Instant::now();
        let mut w = world_with(Some(settings(rules)), true);
        let mut a = seat(&mut w, "A", t0);
        assert_eq!(w.request_encounter(a.entity, Some(0), t0), BattleReply::Done);
        play_out(&mut w, &mut [&mut a], &rules, t0);
        assert_eq!(progress_msgs(&mut a).last(), Some(&(0, 5, 52, 8, 20, 2, false)));
    }

    #[test]
    fn a_won_battle_is_replayable_by_every_member_and_everyone_returns_to_the_field() {
        let rules = SyntheticRules::default();
        let t0 = Instant::now();
        let mut w = world_with(Some(settings(rules)), true);
        let (mut a, mut b) = party_of_two(&mut w, t0);
        let mut bystander = seat(&mut w, "C", t0);
        assert_eq!(w.request_encounter(a.entity, Some(1), t0), BattleReply::Done);
        let ended_at = play_out(&mut w, &mut [&mut a, &mut b], &rules, t0);
        assert!(ended_at > 0);

        // Both members heard the same, in the documented order: start, events, the message with
        // the victory, then the end.
        let (ma, mb) = (a.battle_msgs(), b.battle_msgs());
        assert_eq!(ma, mb);
        assert!(matches!(ma.first(), Some(ServerMsg::EncounterStart { .. })));
        let n = ma.len();
        match (&ma[n - 2], &ma[n - 1]) {
            (ServerMsg::BattleEvent { events, .. }, ServerMsg::BattleEnd { outcome, rewards, .. }) => {
                assert_eq!(events.last(), Some(&BattleEvent::Victory), "the victory is announced before the end");
                assert_eq!(*outcome, fmv_protocol::battle::Outcome::Victory);
                assert_eq!(rewards.len(), 2, "one entry per roster member");
                // Two dummies: 24 experience, 40 gil and 4 AP in total, shared by whoever lived.
                assert!(rewards.iter().all(|r| [a.account, b.account].contains(&r.account) && (r.ap == 4 || r.ap == 0)), "{rewards:?}");
                assert_eq!(rewards.iter().map(|r| r.exp).sum::<u32>(), 24, "{rewards:?}");
                assert_eq!(rewards.iter().map(|r| r.gil).sum::<u32>(), 40, "{rewards:?}");
            }
            other => panic!("the last two messages: {other:?}"),
        }
        // Everything over: no battle, nobody frozen, the bystander never knew.
        assert_eq!(w.battle_count(), 0);
        assert!(!w.in_battle(a.entity) && !w.in_battle(b.entity));
        assert!(bystander.battle_msgs().is_empty());
        // Back in the field: moving works again, and the next tick tells everyone where the members
        // stand (their entities were marked changed).
        w.advance_tick();
        let updates = bystander.drain();
        assert!(updates.iter().any(|m| matches!(m, ServerMsg::EntityUpdate { entities, .. } if entities.iter().any(|e| e.id == a.entity) && entities.iter().any(|e| e.id == b.entity))), "{updates:?}");
        let later = ms(t0, 60_000 * 10 + 1_000);
        assert_eq!(w.apply_move(a.entity, Pos::new(-90.0, 0.0, 0.0), 1.0, AnimState::Walk, later), MoveOutcome::Applied);
        // A new battle can start again.
        assert_eq!(w.request_encounter(a.entity, Some(0), later), BattleReply::Done);
    }

    #[test]
    fn a_lost_battle_ends_with_defeat_and_no_rewards_and_frees_the_party() {
        let rules = SyntheticRules::default();
        let t0 = Instant::now();
        let mut w = world_with(Some(settings(rules)), true);
        let (mut a, mut b) = party_of_two(&mut w, t0);
        assert_eq!(w.request_encounter(a.entity, Some(2), t0), BattleReply::Done);
        play_out(&mut w, &mut [&mut a, &mut b], &rules, t0);
        let msgs = a.battle_msgs();
        match msgs.last() {
            Some(ServerMsg::BattleEnd { outcome, rewards, .. }) => {
                assert_eq!(*outcome, fmv_protocol::battle::Outcome::Defeat);
                assert!(rewards.is_empty());
            }
            other => panic!("{other:?}"),
        }
        assert!(!w.in_battle(a.entity) && !w.in_battle(b.entity));
    }

    /// The rooms with the story point that starts a battle (`RoomsAdapter::with_interactions`), a saver queue to read,
    /// and battles on (`settings`, or none).
    fn rooms_world(settings: Option<BattleSettings>) -> (World, mpsc::UnboundedReceiver<Msg>) {
        use fmv_fixture::rooms::{RoomsAdapter, HALL};
        let adapter: Arc<dyn GameAdapter> = Arc::new(RoomsAdapter::with_interactions());
        let mut w = World::with_party_config(adapter.load_area(HALL).unwrap(), fmv_party::PartyConfig::default()).unwrap();
        w.set_area_source(adapter);
        let (handle, rx) = SaverHandle::channel();
        w.set_progress_saver(handle);
        w.configure_battle(settings, false);
        (w, rx)
    }

    /// Hop to (x, y) 700 ms after `*now`.
    fn hop(w: &mut World, seat: &Seat, x: f32, y: f32, now: &mut Instant) -> MoveOutcome {
        *now += Duration::from_millis(700);
        w.apply_move(seat.entity, Pos::new(x, y, 0.0), 0.0, AnimState::Walk, *now)
    }

    fn progression_of(w: &World, seat: &Seat) -> Vec<u8> {
        w.players.get(&seat.entity).unwrap().progression.clone()
    }

    fn story_lines(w: &mut World) -> Vec<String> {
        w.take_logs().iter().map(|l| l.text().to_string()).filter(|l| l.contains("story ") || l.starts_with("battle ")).collect()
    }

    #[test]
    fn a_story_point_that_starts_a_battle_pays_its_step_when_the_battle_is_won_and_not_before() {
        use fmv_fixture::rooms::{BATTLE_FORMATION, BATTLE_REGION, STORY_ADVANCED};
        let rules = SyntheticRules::default();
        let t0 = Instant::now();
        let (mut w, mut saved) = rooms_world(Some(settings(rules)));
        let mut a = seat(&mut w, "A", t0);
        a.drain();
        w.take_logs();
        let mut now = t0;
        // The way to the battle region (x -300..-250, y 200..250): the story waits for the fight.
        for (x, y) in [(-100.0, 100.0), (-200.0, 180.0)] {
            assert_eq!(hop(&mut w, &a, x, y, &mut now), MoveOutcome::Applied);
        }
        assert_eq!(BATTLE_REGION[0], (-300.0, 200.0));
        assert!(!w.in_battle(a.entity));
        assert_eq!(hop(&mut w, &a, -270.0, 220.0, &mut now), MoveOutcome::Applied);
        assert!(w.in_battle(a.entity), "the region started the battle");
        assert!(progression_of(&w, &a).is_empty(), "the step is the price of the win: nothing yet");
        let started = story_lines(&mut w);
        assert!(started.iter().any(|l| l.starts_with("story battle started battle=1") && l.contains("point=2") && l.contains(&format!("formation={BATTLE_FORMATION}"))), "{started:?}");
        assert!(started.iter().any(|l| l.starts_with("battle started battle=1")), "{started:?}");
        assert!(!started.iter().any(|l| l.contains("story advanced")), "{started:?}");
        assert!(saved_stories_of(&mut saved).is_empty());
        // Won: the step is applied, the player is told after the battle's end, and it is saved.
        play_out(&mut w, &mut [&mut a], &rules, t0);
        assert_eq!(progression_of(&w, &a), STORY_ADVANCED);
        let msgs = a.drain();
        let kinds: Vec<&str> = msgs
            .iter()
            .filter_map(|m| match m {
                ServerMsg::BattleEnd { outcome, .. } => Some(if *outcome == fmv_protocol::battle::Outcome::Victory { "victory" } else { "other" }),
                ServerMsg::StoryUpdate { advance: Some(_), .. } => Some("step"),
                _ => None,
            })
            .collect();
        let after = a.log.iter().filter_map(|m| match m {
            ServerMsg::BattleEnd { .. } => Some("end"),
            ServerMsg::StoryUpdate { advance: Some(_), .. } => Some("step"),
            _ => None,
        });
        assert_eq!(after.collect::<Vec<_>>(), ["end", "step"], "the step comes after the end of the battle ({kinds:?})");
        let lines = story_lines(&mut w);
        assert!(lines.iter().any(|l| l.contains("story advanced") && l.contains("point=2") && l.contains("moment=0->1")), "{lines:?}");
        assert_eq!(saved_stories_of(&mut saved), [STORY_ADVANCED.to_vec()]);
        // Standing in the region now starts nothing (the step is taken), and neither does a second visit.
        assert_eq!(hop(&mut w, &a, -260.0, 230.0, &mut now), MoveOutcome::Applied);
        assert!(!w.in_battle(a.entity));
    }

    fn saved_stories_of(rx: &mut mpsc::UnboundedReceiver<Msg>) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            if let Msg::Story(job) = msg {
                out.push(job.progress);
            }
        }
        out
    }

    #[test]
    fn a_lost_story_battle_leaves_the_story_where_it_was_and_the_region_is_reached_again_only_from_outside() {
        use fmv_fixture::rooms::{LOSING_FORMATION, LOSING_REGION};
        let rules = SyntheticRules::default();
        let t0 = Instant::now();
        let (mut w, mut saved) = rooms_world(Some(settings(rules)));
        let mut a = seat(&mut w, "A", t0);
        a.drain();
        w.take_logs();
        let mut now = t0;
        // The losing region (x 250..300, y -250..-200): the overwhelming enemy.
        assert_eq!(LOSING_REGION[0], (250.0, -250.0));
        for (x, y) in [(0.0, -100.0), (100.0, -180.0), (200.0, -230.0)] {
            assert_eq!(hop(&mut w, &a, x, y, &mut now), MoveOutcome::Applied);
        }
        assert_eq!(hop(&mut w, &a, 270.0, -230.0, &mut now), MoveOutcome::Applied);
        assert!(w.in_battle(a.entity));
        let started = story_lines(&mut w);
        assert!(started.iter().any(|l| l.starts_with("story battle started") && l.contains(&format!("formation={LOSING_FORMATION}"))), "{started:?}");
        play_out(&mut w, &mut [&mut a], &rules, t0);
        assert!(a.log.iter().any(|m| matches!(m, ServerMsg::BattleEnd { outcome: fmv_protocol::battle::Outcome::Defeat, .. })));
        assert!(!w.in_battle(a.entity), "the party is back in the field");
        assert!(progression_of(&w, &a).is_empty(), "a defeat leaves the story where it was");
        assert!(saved_stories_of(&mut saved).is_empty());
        assert!(!story_lines(&mut w).iter().any(|l| l.contains("story advanced")));
        // The player stands in the region still: its next step inside it does not start the fight again...
        let later = ms(t0, 60_000 * 10);
        assert_eq!(w.apply_move(a.entity, Pos::new(275.0, -225.0, 0.0), 0.0, AnimState::Walk, later), MoveOutcome::Applied);
        assert!(!w.in_battle(a.entity));
        // ...but coming to it from outside does
        assert_eq!(w.apply_move(a.entity, Pos::new(200.0, -230.0, 0.0), 0.0, AnimState::Walk, ms(t0, 60_000 * 10 + 700)), MoveOutcome::Applied);
        assert_eq!(w.apply_move(a.entity, Pos::new(270.0, -230.0, 0.0), 0.0, AnimState::Walk, ms(t0, 60_000 * 10 + 1_400)), MoveOutcome::Applied);
        assert!(w.in_battle(a.entity));
    }

    #[test]
    fn a_server_that_runs_no_battles_cannot_pay_the_price_so_the_story_waits_and_says_so() {
        let t0 = Instant::now();
        let (mut w, mut saved) = rooms_world(None);
        let mut a = seat(&mut w, "A", t0);
        a.drain();
        w.take_logs();
        let mut now = t0;
        for (x, y) in [(-100.0, 100.0), (-200.0, 180.0), (-270.0, 220.0)] {
            assert_eq!(hop(&mut w, &a, x, y, &mut now), MoveOutcome::Applied);
        }
        assert!(!w.in_battle(a.entity));
        assert!(progression_of(&w, &a).is_empty(), "the step is never taken without the fight");
        assert!(saved_stories_of(&mut saved).is_empty());
        let lines = story_lines(&mut w);
        assert!(lines.iter().any(|l| l.starts_with("a story battle could not start, the story waits") && l.contains("point=2") && l.contains("Unsupported")), "{lines:?}");
    }

    #[test]
    fn a_member_of_a_party_that_may_not_advance_the_story_does_not_start_the_story_battle() {
        let rules = SyntheticRules::default();
        let t0 = Instant::now();
        let (mut w, _saved) = rooms_world(Some(settings(rules)));
        // A leads with the advanced story; B joins its party with a new account's story
        let (mut a, mut b) = (seat_as(&mut w, "A", t0, ProgressSeed::default()), seat_as(&mut w, "B", t0, ProgressSeed::default()));
        w.players.get_mut(&a.entity).unwrap().progression = fmv_fixture::rooms::STORY_ADVANCED.to_vec();
        assert_eq!(w.party_invite(a.entity, b.entity, t0), crate::world::PartyOutcome::Done);
        let invite = w.parties.invites().next().expect("an invite").id;
        assert_eq!(w.party_accept(b.entity, invite, t0), crate::world::PartyOutcome::Done);
        a.drain();
        b.drain();
        let mut now = t0;
        for (x, y) in [(100.0, 100.0), (0.0, 180.0), (-200.0, 190.0), (-270.0, 220.0)] {
            assert_eq!(hop(&mut w, &b, x, y, &mut now), MoveOutcome::Applied);
        }
        assert!(!w.in_battle(b.entity) && !w.in_battle(a.entity), "the member's own story may not advance in the leader's phase");
        assert_eq!(w.battle_count(), 0);
    }

    #[test]
    fn a_member_who_leaves_stays_in_the_engine_and_the_battle_goes_on_until_nobody_is_left() {
        let rules = SyntheticRules::default();
        let t0 = Instant::now();
        let mut w = world_with(Some(settings(rules)), true);
        let (mut a, b) = party_of_two(&mut w, t0);
        assert_eq!(w.request_encounter(a.entity, Some(1), t0), BattleReply::Done);
        let (a_entity, b_entity) = (a.entity, b.entity);
        drop(b);
        assert!(w.leave(b_entity));
        assert_eq!(w.battle_count(), 1, "A is still there");
        // A can still win it alone.
        play_out(&mut w, &mut [&mut a], &rules, t0);
        assert_eq!(w.battle_count(), 0);
        assert!(!w.in_battle(a_entity));

        // The last member leaving drops the battle.
        let mut w = world_with(Some(settings(rules)), true);
        let a = seat(&mut w, "A", t0);
        assert_eq!(w.request_encounter(a.entity, Some(0), t0), BattleReply::Done);
        assert_eq!(w.battle_count(), 1);
        assert!(w.leave(a.entity));
        assert_eq!(w.battle_count(), 0, "nobody left to see it");
        w.advance_battles(ms(t0, 1_000)); // and nothing is left to step
    }

    #[test]
    fn the_engine_consumes_exactly_the_time_that_passed() {
        let rules = SyntheticRules::default();
        let t0 = Instant::now();
        let mut w = world_with(Some(settings(rules)), true);
        let a = seat(&mut w, "A", t0);
        assert_eq!(w.request_encounter(a.entity, Some(0), t0), BattleReply::Done);
        // 1000 ticks per second: 0.0345 s of server time is 34 ticks and a fraction that is kept.
        w.advance_battles(t0 + Duration::from_micros(34_500));
        let tick = |w: &World| w.battle.as_ref().unwrap().battles.get(&BattleId(1)).unwrap().state.tick();
        assert_eq!(tick(&w), 34);
        w.advance_battles(t0 + Duration::from_micros(69_000));
        assert_eq!(tick(&w), 69, "no millisecond is lost to rounding between steps");
        // A step in the past, or the same instant, changes nothing.
        w.advance_battles(t0);
        assert_eq!(tick(&w), 69);
        // A stall far longer than the engine takes in one step does not leave a backlog: one more
        // 10 ms moves the engine by about 10 ticks, not by the hour that was dropped.
        w.advance_battles(t0 + Duration::from_secs(3_600));
        let now_tick = |w: &World| w.battle.as_ref().and_then(|b| b.battles.get(&BattleId(1))).map(|b| b.state.tick());
        let before = now_tick(&w);
        w.advance_battles(t0 + Duration::from_secs(3_600) + Duration::from_millis(10));
        if let (Some(before), Some(after)) = (before, now_tick(&w)) {
            assert!(after - before <= 10, "{before} -> {after}");
        }
    }

    // ---- random encounters ----

    /// The fixture plaza with a random-encounter table.
    struct Wild {
        inner: Box<dyn AreaRuntime>,
        table: Option<EncounterTable>,
    }

    impl AreaRuntime for Wild {
        fn info(&self) -> &AreaInfo {
            self.inner.info()
        }
        fn spawn_points(&self) -> Vec<Pos> {
            self.inner.spawn_points()
        }
        fn validate_move(&self, from: Pos, to: Pos) -> MoveResult {
            self.inner.validate_move(from, to)
        }
        fn bounds(&self) -> Bounds {
            self.inner.bounds()
        }
        fn walk_triangles(&self) -> Vec<[Pos; 3]> {
            self.inner.walk_triangles()
        }
        fn max_speed(&self) -> f32 {
            self.inner.max_speed()
        }
        fn encounters(&self) -> Option<EncounterTable> {
            self.table.clone()
        }
    }

    /// The plaza with `table`. Random encounters need no debug flag, so most tests leave it off.
    fn wild_world(table: Option<EncounterTable>, settings: Option<BattleSettings>, debug: bool) -> World {
        let inner = FixtureAdapter::new().load_area(FIXTURE_AREA_KEY).unwrap();
        let mut w = World::new(Box::new(Wild { inner, table })).unwrap();
        w.configure_battle(settings, debug);
        w
    }

    /// Walk back and forth on the plaza (always affordable: 80 units every 200 ms is far below
    /// the speed limit) until a battle starts or the steps run out.
    fn pace(w: &mut World, a: &Seat, t0: Instant, steps: u64) -> Option<u64> {
        for i in 1..=steps {
            let x = if i % 2 == 0 { -100.0 } else { -20.0 };
            let now = ms(t0, i * 200);
            let _ = w.apply_move(a.entity, Pos::new(x, 0.0, 0.0), 0.0, AnimState::Walk, now);
            if w.in_battle(a.entity) {
                return Some(i);
            }
        }
        None
    }

    fn table(rate: u8) -> EncounterTable {
        EncounterTable {
            rate,
            entries: vec![EncounterEntry { formation: 0, weight: 32 }, EncounterEntry { formation: 1, weight: 32 }],
        }
    }

    #[test]
    fn walking_in_an_area_with_a_table_eventually_starts_a_random_battle_with_a_formation_of_the_table() {
        let t0 = Instant::now();
        let mut s = settings(SyntheticRules::default());
        s.encounter_step_units = Some(40.0); // a roll every half move
        let mut w = wild_world(Some(table(255)), Some(s), false);
        let mut a = seat(&mut w, "A", t0);
        a.drain();
        let started = pace(&mut w, &a, t0, 400).expect("a 25% roll per half move triggers within 400 moves");
        assert!(started >= 1);
        match &a.battle_msgs()[..] {
            [ServerMsg::EncounterStart { formation_id, party, .. }] => {
                assert!([0, 1].contains(formation_id), "a formation of the table: {formation_id}");
                assert_eq!(party.len(), 1);
            }
            other => panic!("{other:?}"),
        }
        // Frozen now: more walking neither moves nor starts a second battle.
        assert_eq!(w.battle_count(), 1);
        assert_eq!(w.apply_move(a.entity, Pos::new(0.0, 0.0, 0.0), 0.0, AnimState::Walk, ms(t0, 100_000)), MoveOutcome::Frozen);
    }

    #[test]
    fn no_table_no_rate_or_no_battle_support_means_no_random_battles() {
        let t0 = Instant::now();
        let mut s = settings(SyntheticRules::default());
        s.encounter_step_units = Some(10.0);
        // The area has no table.
        let mut w = wild_world(None, Some(s.clone()), false);
        let a = seat(&mut w, "A", t0);
        assert_eq!(pace(&mut w, &a, t0, 300), None);
        // A table with rate 0, and one with only zero weights, are empty tables.
        for t in [table(0), EncounterTable { rate: 255, entries: vec![EncounterEntry { formation: 0, weight: 0 }] }] {
            let mut w = wild_world(Some(t), Some(s.clone()), false);
            let a = seat(&mut w, "A", t0);
            assert_eq!(pace(&mut w, &a, t0, 300), None);
        }
        // A server without battle data walks on in peace even in an area with a table.
        let mut w = wild_world(Some(table(255)), None, false);
        let a = seat(&mut w, "A", t0);
        assert_eq!(pace(&mut w, &a, t0, 300), None);
        assert_eq!(w.battle_count(), 0);
    }

    #[test]
    fn a_table_that_names_a_formation_the_rules_do_not_know_starts_nothing() {
        let t0 = Instant::now();
        let mut s = settings(SyntheticRules::default());
        s.encounter_step_units = Some(10.0);
        let broken = EncounterTable { rate: 255, entries: vec![EncounterEntry { formation: 9_999, weight: 64 }] };
        let mut w = wild_world(Some(broken), Some(s), false);
        let a = seat(&mut w, "A", t0);
        assert_eq!(pace(&mut w, &a, t0, 300), None);
        assert_eq!(w.battle_count(), 0);
    }

    #[test]
    fn the_debug_default_formation_prefers_the_operators_choice_then_the_table_then_the_rules() {
        let t0 = Instant::now();
        // Operator's choice wins over everything.
        let mut s = settings(SyntheticRules::default());
        s.debug_formation = Some(3);
        let mut w = wild_world(Some(table(10)), Some(s), true);
        let mut a = seat(&mut w, "A", t0);
        assert_eq!(w.request_encounter(a.entity, None, t0), BattleReply::Done);
        let formation = a.battle_msgs().iter().find_map(|m| match m {
            ServerMsg::EncounterStart { formation_id, preemptive, .. } => Some((*formation_id, *preemptive)),
            _ => None,
        });
        assert_eq!(formation, Some((3, true)), "formation 3 has a preemptive layout, announced in the start");

        // Without a choice: the table (formation 0 or 1 here), else the rules' default (0).
        let mut w = wild_world(Some(table(10)), Some(settings(SyntheticRules::default())), true);
        let mut a = seat(&mut w, "A", t0);
        assert_eq!(w.request_encounter(a.entity, None, t0), BattleReply::Done);
        let f = a.battle_msgs().iter().find_map(|m| match m {
            ServerMsg::EncounterStart { formation_id, .. } => Some(*formation_id),
            _ => None,
        });
        assert!(matches!(f, Some(0 | 1)), "{f:?}");
    }

    #[test]
    fn with_random_encounters_off_a_debug_request_still_picks_from_the_areas_table_and_walking_rolls_nothing() {
        // Regression: `--no-random-encounters` used to drop the area's table entirely, so a debug
        // request that named no formation fell through to the rules' default formation (in FF7 a
        // one-enemy formation that pays nothing) instead of the field's own table.
        let t0 = Instant::now();
        let mut s = settings(SyntheticRules::default());
        s.random_encounters = false;
        s.encounter_step_units = Some(10.0);
        // A table naming only formation 1, so the rules' default (0) is distinguishable.
        let only_one = EncounterTable { rate: 255, entries: vec![EncounterEntry { formation: 1, weight: 64 }] };
        let mut w = wild_world(Some(only_one.clone()), Some(s.clone()), true);
        let mut a = seat(&mut w, "A", t0);
        // Walking with random encounters off starts nothing, even at rate 255.
        assert_eq!(pace(&mut w, &a, t0, 300), None);
        assert_eq!(w.battle_count(), 0);
        a.drain();
        assert_eq!(w.request_encounter(a.entity, None, ms(t0, 120_000)), BattleReply::Done);
        let f = a.battle_msgs().iter().find_map(|m| match m {
            ServerMsg::EncounterStart { formation_id, .. } => Some(*formation_id),
            _ => None,
        });
        assert_eq!(f, Some(1), "the area's table, not the rules' default");
    }

    // ---------------------------------------------------------------------------------------------
    // Canon companions (protocol v9): the story's party first, commanded by the server, then the players
    // ---------------------------------------------------------------------------------------------

    /// The fixture's plaza with a story: `story_party` answers `party(progression)`, the canon party at that story
    /// state. Everything else is the plaza's.
    struct StoryArea {
        inner: Box<dyn AreaRuntime>,
        party: fn(&[u8]) -> Vec<u8>,
    }

    impl AreaRuntime for StoryArea {
        fn info(&self) -> &AreaInfo {
            self.inner.info()
        }
        fn spawn_points(&self) -> Vec<Pos> {
            self.inner.spawn_points()
        }
        fn validate_move(&self, from: Pos, to: Pos) -> MoveResult {
            self.inner.validate_move(from, to)
        }
        fn bounds(&self) -> Bounds {
            self.inner.bounds()
        }
        fn walk_triangles(&self) -> Vec<[Pos; 3]> {
            self.inner.walk_triangles()
        }
        fn max_speed(&self) -> f32 {
            self.inner.max_speed()
        }
        fn story_party(&self, progress: &[u8]) -> Vec<u8> {
            (self.party)(progress)
        }
    }

    /// Cloud and Barret whatever the story state: the first reactor mission's canon party.
    fn cloud_and_barret(_: &[u8]) -> Vec<u8> {
        vec![0, 1]
    }

    /// A world over the plaza whose story's canon party is `party(progression)`, with battles on.
    fn world_with_party(settings: BattleSettings, party: fn(&[u8]) -> Vec<u8>) -> World {
        let inner = FixtureAdapter::new().load_area(FIXTURE_AREA_KEY).unwrap();
        let mut w = World::new(Box::new(StoryArea { inner, party })).unwrap();
        w.configure_battle(Some(settings), true);
        w
    }

    /// A player with a story `progression` (what the canon party is read from) and character progress `progress`.
    fn seat_story(w: &mut World, name: &str, now: Instant, progress: ProgressSeed, progression: Vec<u8>) -> Seat {
        let (session, entity) = w.allocate_ids();
        let (tx, rx) = mpsc::channel(4096);
        let close = Arc::new(watch::channel(false).0);
        let avatar = AvatarAppearance { display_name: name.into(), tint: [1, 2, 3], appearance_ref: None };
        let account = AccountId(entity.0);
        w.join(NewPlayer { session, entity, account, avatar, progression, progress, tx, close }, now).expect("joins");
        Seat { entity, account, rx, log: Vec::new() }
    }

    /// `leader` invites `member` and `member` accepts (the leader is the first member of the party).
    fn party_up(w: &mut World, leader: &Seat, member: &Seat, now: Instant) {
        assert_eq!(w.party_invite(leader.entity, member.entity, now), crate::world::PartyOutcome::Done);
        let invite = w.parties.invites().next().expect("an invite").id;
        assert_eq!(w.party_accept(member.entity, invite, now), crate::world::PartyOutcome::Done);
    }

    /// The roster of the `EncounterStart` a seat was sent last.
    fn roster_of(seat: &mut Seat) -> Vec<BattleMember> {
        seat.battle_msgs()
            .iter()
            .rev()
            .find_map(|m| match m {
                ServerMsg::EncounterStart { party, .. } => Some(party.clone()),
                _ => None,
            })
            .expect("an EncounterStart")
    }

    /// `(controller kind, name, character, level)` of every member: the roster as a player's screen shows it.
    fn who(party: &[BattleMember]) -> Vec<(&'static str, String, u8, u8)> {
        party.iter().map(|m| (if m.is_companion() { "companion" } else { "player" }, m.name.clone(), m.character, m.level)).collect()
    }

    #[test]
    fn a_solo_player_fights_beside_the_canon_party_which_goes_first_and_nobody_is_replaced() {
        let t0 = Instant::now();
        let mut w = world_with_party(settings(SyntheticRules::default()), cloud_and_barret);
        // A new character on Tifa's curve (2), at the synthetic start (level 5): it is ADDED after Cloud and Barret.
        let mut a = seat_as(&mut w, "Aria", t0, ProgressSeed::of(2, None));
        a.drain();
        assert_eq!(w.request_encounter(a.entity, Some(1), t0), BattleReply::Done);
        let party = roster_of(&mut a);
        assert_eq!(
            who(&party),
            vec![("companion", "Cloud".to_string(), 0, 5), ("companion", "Barret".to_string(), 1, 5), ("player", "Aria".to_string(), 2, 5)]
        );
        // Only the player has an entity and an account; the companions have none and nobody can be frozen for them.
        assert_eq!(party.iter().map(BattleMember::entity).collect::<Vec<_>>(), vec![None, None, Some(a.entity)]);
        assert_eq!(party.iter().map(BattleMember::account).collect::<Vec<_>>(), vec![None, None, Some(a.account)]);
        assert!(w.in_battle(a.entity));
        // The engine fights them as those canon characters' templates: Cloud, Barret and a Tifa-curve player.
        let engine = &w.battle.as_ref().unwrap().battles.get(&BattleId(1)).unwrap().state;
        assert_eq!(engine.party_ids(), vec![ActorId(0), ActorId(1), ActorId(2)]);
        assert_eq!(engine.actors().iter().take(3).map(|c| c.name.as_str()).collect::<Vec<_>>(), ["Cloud", "Barret", "Tifa"]);
        // The line the smoke scripts read: members is the number of players, companions come first in every list.
        let started = logs_of(&mut w).into_iter().find(|l| l.starts_with("battle started")).expect("a start line");
        assert!(started.contains(" members=1 ") && started.contains(" companions=2"), "{started}");
        assert!(started.contains("characters=[0, 1, 2]") && started.contains("levels=[5, 5, 5]"), "{started}");
        assert!(started.contains("names=[Cloud,Barret,Aria]"), "{started}");
    }

    /// The owner's own cases, and every other: whatever a player's internal curve is, a solo player fights as
    /// [Cloud, Barret, the player]. A new character on Barret's curve (a "Blizzy": created with slot 1) must not make Barret
    /// the player's model or drop the canon Barret; an account that predates the creator, on Cloud's curve (slot 0, like
    /// the owner's Blizz), must not drop Cloud or give a second one. Neither canon member is ever dropped or replaced.
    #[test]
    fn neither_canon_member_is_ever_dropped_or_replaced_whatever_a_players_curve_is() {
        let t0 = Instant::now();
        for curve in 0..=8u8 {
            let mut w = world_with_party(settings(SyntheticRules::default()), cloud_and_barret);
            let mut a = seat_as(&mut w, "Blizzy", t0, ProgressSeed::of(curve, None));
            a.drain();
            assert_eq!(w.request_encounter(a.entity, Some(1), t0), BattleReply::Done, "curve {curve}");
            let party = roster_of(&mut a);
            assert_eq!(
                party.iter().map(|m| (m.is_companion(), m.name.as_str(), m.character)).collect::<Vec<_>>(),
                vec![(true, "Cloud", 0), (true, "Barret", 1), (false, "Blizzy", curve)],
                "curve {curve}: the canon pair first and untouched, the player added with its own name and its curve"
            );
            // The engine fights the player with the curve's template and the canon pair with their own.
            let engine = &w.battle.as_ref().unwrap().battles.get(&BattleId(1)).unwrap().state;
            let canon = fmv_ff7_battle::kernel::CharacterId::from_index(usize::from(curve)).map(|c| c.name());
            assert_eq!(engine.actors().iter().take(3).map(|c| c.name.as_str()).collect::<Vec<_>>(), ["Cloud", "Barret", canon.unwrap_or("?")], "curve {curve}");
            // Exactly one entity is frozen (the player's): a companion has none.
            assert_eq!(party.iter().filter_map(BattleMember::entity).collect::<Vec<_>>(), vec![a.entity]);
        }
    }

    #[test]
    fn two_players_fight_as_cloud_barret_then_the_players_in_the_partys_own_order() {
        let t0 = Instant::now();
        let mut w = world_with_party(settings(SyntheticRules::default()), cloud_and_barret);
        // B connected first, so its entity is the lower; A leads the party (A invited B), so A is first in the party.
        let b = seat_as(&mut w, "Brom", t0, ProgressSeed::of(2, None));
        let mut a = seat_as(&mut w, "Aria", t0, ProgressSeed::of(1, None));
        assert!(b.entity < a.entity);
        party_up(&mut w, &a, &b, t0);
        let mut b = b;
        a.drain();
        b.drain();
        // The member asks: the whole party goes.
        assert_eq!(w.request_encounter(b.entity, Some(1), t0), BattleReply::Done);
        let (pa, pb) = (roster_of(&mut a), roster_of(&mut b));
        assert_eq!(pa, pb, "everybody is told the same roster");
        assert_eq!(
            who(&pa),
            vec![
                ("companion", "Cloud".to_string(), 0, 5),
                ("companion", "Barret".to_string(), 1, 5),
                ("player", "Aria".to_string(), 1, 5),
                ("player", "Brom".to_string(), 2, 5)
            ],
            "the canon party, then the players in the party's order (the leader first), not in entity order"
        );
        assert!(w.in_battle(a.entity) && w.in_battle(b.entity));
        let started = logs_of(&mut w).into_iter().find(|l| l.starts_with("battle started")).expect("a start line");
        assert!(started.contains(" members=2 ") && started.contains(" companions=2") && started.contains("names=[Cloud,Barret,Aria,Brom]"), "{started}");
    }

    #[test]
    fn three_players_and_the_canon_party_make_five_fighters() {
        let t0 = Instant::now();
        let mut w = world_with_party(settings(SyntheticRules::default()), cloud_and_barret);
        let (mut a, b, c) = (seat(&mut w, "A", t0), seat(&mut w, "B", t0), seat(&mut w, "C", t0));
        party_up(&mut w, &a, &b, t0);
        party_up(&mut w, &a, &c, t0);
        a.drain();
        assert_eq!(w.request_encounter(a.entity, Some(1), t0), BattleReply::Done);
        let party = roster_of(&mut a);
        assert_eq!(party.len(), 5);
        assert_eq!(party.iter().map(|m| m.is_companion()).collect::<Vec<_>>(), [true, true, false, false, false]);
        assert_eq!(party.iter().filter_map(BattleMember::entity).collect::<Vec<_>>(), vec![a.entity, b.entity, c.entity]);
        let engine = &w.battle.as_ref().unwrap().battles.get(&BattleId(1)).unwrap().state;
        assert_eq!((engine.party_ids().len(), engine.enemy_ids()[0]), (5, ActorId(5)), "the enemies follow the five");
    }

    /// The canon party is the story of the party's LEADER (every member of a party is in the leader's instance) and the
    /// companions fight at the leader's level, whoever asks; `--battle-level` pins the level of everybody.
    #[test]
    fn the_canon_party_and_its_level_are_the_party_leaders_whoever_asks() {
        // The story states: [1] is the first mission (Cloud and Barret), [2] a story with Cloud alone, anything else none.
        fn by_story(progress: &[u8]) -> Vec<u8> {
            match progress {
                [1] => vec![0, 1],
                [2] => vec![0],
                _ => vec![],
            }
        }
        let t0 = Instant::now();
        let mut w = world_with_party(settings(SyntheticRules::default()), by_story);
        // Level 9 starts at 144 experience on the synthetic curve.
        let nine = ProgressSeed::of(0, Some(Progress { character: 0, level: 9, exp: 144, ap: 0, gil: 0, limit: 0 }));
        let mut a = seat_story(&mut w, "Lead", t0, nine, vec![1]);
        let mut b = seat_story(&mut w, "Follow", t0, ProgressSeed::of(1, None), vec![2]);
        party_up(&mut w, &a, &b, t0);
        a.drain();
        b.drain();
        // B (a member whose own story has Cloud alone, at level 5) asks: the LEADER's story and level decide.
        assert_eq!(w.request_encounter(b.entity, Some(1), t0), BattleReply::Done);
        let party = roster_of(&mut b);
        assert_eq!(
            who(&party),
            vec![
                ("companion", "Cloud".to_string(), 0, 9),
                ("companion", "Barret".to_string(), 1, 9),
                ("player", "Lead".to_string(), 0, 9),
                ("player", "Follow".to_string(), 1, 5)
            ],
            "Cloud and Barret from the leader's story, at the leader's level 9; each player at its own"
        );
        // Alone, with no party, the player's own story and level are the ones.
        let mut c = seat_story(&mut w, "Solo", t0, ProgressSeed::of(2, None), vec![2]);
        c.drain();
        assert_eq!(w.request_encounter(c.entity, Some(0), t0), BattleReply::Done);
        assert_eq!(who(&roster_of(&mut c)), vec![("companion", "Cloud".to_string(), 0, 5), ("player", "Solo".to_string(), 2, 5)]);

        // `--battle-level` pins the level of the whole fight, the companions' too.
        let mut pinned = settings(SyntheticRules::default());
        pinned.level = Some(12);
        let mut w = world_with_party(pinned, cloud_and_barret);
        let mut d = seat_as(&mut w, "Pin", t0, ProgressSeed::default());
        d.drain();
        assert_eq!(w.request_encounter(d.entity, Some(0), t0), BattleReply::Done);
        assert_eq!(roster_of(&mut d).iter().map(|m| m.level).collect::<Vec<_>>(), [12, 12, 12]);
    }

    #[test]
    fn a_story_with_no_canon_party_or_a_rule_set_that_names_nobody_fights_with_the_players_only() {
        let t0 = Instant::now();
        // No canon party in the story (the plaza's own default).
        let mut w = world_with(Some(settings(SyntheticRules::default())), true);
        let mut a = seat(&mut w, "A", t0);
        a.drain();
        assert_eq!(w.request_encounter(a.entity, Some(0), t0), BattleReply::Done);
        assert_eq!(who(&roster_of(&mut a)), vec![("player", "A".to_string(), 0, 5)]);
        // An id the rules know nobody by is left out rather than invented.
        fn odd(_: &[u8]) -> Vec<u8> {
            vec![0, 200, 1]
        }
        let mut w = world_with_party(settings(SyntheticRules::default()), odd);
        let mut b = seat(&mut w, "B", t0);
        b.drain();
        assert_eq!(w.request_encounter(b.entity, Some(0), t0), BattleReply::Done);
        let names: Vec<String> = roster_of(&mut b).iter().map(|m| m.name.clone()).collect();
        assert_eq!(names, ["Cloud", "Barret", "B"]);
    }

    /// Preemptive start: every gauge is full at once. With pacing off the companions act on the server's first step, as
    /// ordinary commands the players are told about; no player can command a companion; the player's own turn is still
    /// its own.
    #[test]
    fn the_companions_act_through_ordinary_commands_applied_and_announced_and_no_player_can_command_one() {
        let t0 = Instant::now();
        let mut w = world_with_party(settings_unpaced(SyntheticRules::default()), cloud_and_barret);
        let mut a = seat(&mut w, "Aria", t0);
        a.drain();
        assert_eq!(w.request_encounter(a.entity, Some(3), t0), BattleReply::Done);
        let id = BattleId(1);
        // Right after the start nobody has acted: the engine has not run a tick.
        assert!(a.battle_msgs().iter().all(|m| !matches!(m, ServerMsg::BattleEvent { command: Some(_), .. })));
        // The first step: the gauges were full, so Cloud and Barret each attack the dummy (actor 3), in actor order.
        w.advance_battles(ms(t0, 10));
        let acted: Vec<(ActorId, Command)> = a
            .battle_msgs()
            .iter()
            .filter_map(|m| match m {
                ServerMsg::BattleEvent { command: Some(c), .. } => Some((c.actor, c.command)),
                _ => None,
            })
            .collect();
        assert_eq!(acted, vec![(ActorId(0), Command::Attack { target: ActorId(3) }), (ActorId(1), Command::Attack { target: ActorId(3) })]);
        // The log says so, once for each, with the companion's own name and the command.
        let logs = logs_of(&mut w);
        let lines: Vec<&String> = logs.iter().filter(|l| l.starts_with("companion acted")).collect();
        assert_eq!(lines.len(), 2, "{logs:?}");
        assert_eq!(lines[0], "companion acted battle=1 actor=0 name=Cloud command=attack target=3 character=0 held_ms=0");
        assert_eq!(lines[1], "companion acted battle=1 actor=1 name=Barret command=attack target=3 character=1 held_ms=0");
        // They are counted for their actors, the player has not acted.
        assert_eq!(w.battle.as_ref().unwrap().battles.get(&id).unwrap().commands, vec![1, 1, 0]);
        // No player may command a companion (whatever the gauge says), and a companion's turn is nobody's to take.
        let enemy = ActorId(3);
        assert_eq!(w.battle_command(a.entity, id, ActorId(0), Command::Attack { target: enemy }, ms(t0, 10)), refused(Some(id), BattleRejection::NotYourActor));
        assert_eq!(w.battle_command(a.entity, id, ActorId(1), Command::Defend, ms(t0, 10)), refused(Some(id), BattleRejection::NotYourActor));
        // The player's own actor is ready (preemptive) and takes its own turn as before.
        assert_eq!(w.battle_command(a.entity, id, ActorId(2), Command::Attack { target: enemy }, ms(t0, 10)), BattleReply::Done);
    }

    #[test]
    fn every_member_replays_a_battle_with_companions_exactly_and_only_the_players_are_paid_and_saved() {
        let rules = SyntheticRules::default();
        let t0 = Instant::now();
        let (mut w, mut rx) = {
            let mut w = world_with_party(settings(rules), cloud_and_barret);
            let (handle, rx) = SaverHandle::channel();
            w.set_progress_saver(handle);
            (w, rx)
        };
        let (mut a, mut b) = (seat(&mut w, "Aria", t0), seat(&mut w, "Brom", t0));
        party_up(&mut w, &a, &b, t0);
        a.drain();
        b.drain();
        assert_eq!(w.request_encounter(a.entity, Some(1), t0), BattleReply::Done);
        // `play_out` replays every message of both seats on engines built from their own rosters and checks the end
        // against them: a divergence of any companion command, or a different split of the rewards, panics.
        play_out(&mut w, &mut [&mut a, &mut b], &rules, t0);
        let want_accounts = vec![a.account, b.account];
        for seat in [&a, &b] {
            let rewards = seat
                .log
                .iter()
                .find_map(|m| match m {
                    ServerMsg::BattleEnd { outcome: Outcome::Victory, rewards, .. } => Some(rewards.clone()),
                    _ => None,
                })
                .expect("the battle was won");
            // Two dummies: 24 experience, 40 gil, 4 AP, split among the two living PLAYERS exactly as without companions:
            // the companions take no share.
            let accounts: Vec<AccountId> = rewards.iter().map(|r| r.account).collect();
            assert_eq!(accounts, want_accounts, "one payment per player, in the party's order");
            assert!(rewards.iter().all(|r| (r.exp, r.gil, r.ap) == (12, 20, 4) || (r.exp, r.gil, r.ap) == (0, 0, 0)), "{rewards:?}");
        }
        // The companions were really in it: they gave commands, and the log counts every fighter's.
        let logs = logs_of(&mut w);
        let acted = logs.iter().filter(|l| l.starts_with("companion acted")).count();
        assert!(acted >= 1, "{logs:?}");
        let ended = logs.iter().find(|l| l.starts_with("battle ended")).expect("an end line");
        assert!(ended.contains(" members=2 ") && ended.contains(" companions=2"), "{ended}");
        let commands = ended.split("commands=[").nth(1).and_then(|rest| rest.split(']').next()).expect("commands").to_string();
        assert_eq!(commands.split(", ").count(), 4, "one count per fighter, companions first: {ended}");
        // Saved: the payments of the two players, nothing for a companion, and no limit gauge (players have none, a
        // companion's is never stored).
        let jobs = saved_all(&mut rx);
        assert!(limits_of(&jobs).is_empty(), "{jobs:?}");
        let mut paid: Vec<AccountId> = jobs.iter().filter(|j| j.paid().is_some()).map(|j| j.account).collect();
        paid.sort();
        let mut want = vec![a.account, b.account];
        want.sort();
        assert_eq!(paid, want);
        assert!(jobs.iter().all(|j| j.account == a.account || j.account == b.account), "{jobs:?}");
    }

    /// A companion's blow can end the battle: the players are still told, paid and freed, though they never acted.
    #[test]
    fn a_companions_blow_can_win_the_battle_and_the_player_who_never_acted_is_paid() {
        let rules = SyntheticRules { enemy_hp: 1, ..SyntheticRules::default() };
        let t0 = Instant::now();
        let mut w = world_with_party(settings_unpaced(rules), cloud_and_barret);
        let (handle, mut rx) = SaverHandle::channel();
        w.set_progress_saver(handle);
        let mut a = seat(&mut w, "Aria", t0);
        a.drain();
        // Formation 3 is preemptive with one dummy: Cloud's first attack on the server's first step kills it.
        assert_eq!(w.request_encounter(a.entity, Some(3), t0), BattleReply::Done);
        w.advance_battles(ms(t0, 10));
        assert_eq!(w.battle_count(), 0, "the battle is over");
        assert!(!w.in_battle(a.entity), "the player is back in the field");
        let msgs = a.battle_msgs();
        let end = msgs.iter().find_map(|m| match m {
            ServerMsg::BattleEnd { outcome, rewards, .. } => Some((*outcome, rewards.clone())),
            _ => None,
        });
        assert_eq!(end, Some((Outcome::Victory, vec![BattleReward::new(a.account, 12, 20, 2)])), "one dummy's worth, all to the one living player");
        // Cloud gave the blow, Barret never got a turn, and the player never acted.
        let acted: Vec<ActorId> = msgs
            .iter()
            .filter_map(|m| match m {
                ServerMsg::BattleEvent { command: Some(c), .. } => Some(c.actor),
                _ => None,
            })
            .collect();
        assert_eq!(acted, vec![ActorId(0)]);
        let logs = logs_of(&mut w);
        let ended = logs.iter().find(|l| l.starts_with("battle ended")).expect("an end line");
        assert!(ended.contains("outcome=Victory") && ended.contains("commands=[1, 0, 0]"), "{ended}");
        // The player was paid and saved, a companion never.
        let jobs = saved(&mut rx);
        assert_eq!(jobs.len(), 1);
        assert_eq!((jobs[0].account, jobs[0].paid().map(|r| r.exp)), (a.account, Some(12)));
    }

    /// Under `--debug-limit-full` a companion's gauge is full, so its limit break is the first thing it gives (nobody is
    /// hurt); the line says which limit and nothing is stored, as ever under that flag.
    #[test]
    fn a_companion_with_a_full_limit_gauge_gives_its_limit_first() {
        let t0 = Instant::now();
        let mut s = settings_unpaced(SyntheticRules::default());
        s.debug_limit_full = true;
        let mut w = world_with_party(s, cloud_and_barret);
        let (handle, mut rx) = SaverHandle::channel();
        w.set_progress_saver(handle);
        let mut a = seat(&mut w, "Aria", t0);
        a.drain();
        assert_eq!(w.request_encounter(a.entity, Some(3), t0), BattleReply::Done);
        w.advance_battles(ms(t0, 10));
        let logs = logs_of(&mut w);
        let started = logs.iter().find(|l| l.starts_with("battle started")).expect("a start line");
        assert!(started.contains("limits=[255, 255, 255]"), "every fighter starts full under the flag: {started}");
        let lines: Vec<&String> = logs.iter().filter(|l| l.starts_with("companion acted")).collect();
        assert_eq!(lines[0], "companion acted battle=1 actor=0 name=Cloud command=limit attack=128 target=3 character=0 held_ms=0");
        assert_eq!(lines[1], "companion acted battle=1 actor=1 name=Barret command=limit attack=128 target=3 character=1 held_ms=0");
        // Two limits are more than the dummy has (the battle is won on the spot and the player is paid), but no gauge is
        // stored: not the companions' and not the made-up one of the player.
        let jobs = saved_all(&mut rx);
        assert!(limits_of(&jobs).is_empty(), "nothing is saved for a gauge that was made up: {jobs:?}");
        assert!(logs.iter().any(|l| l.starts_with("limit not kept battle=1 reason=debug-limit-full")), "{logs:?}");
    }

    /// The canon companions do not keep a battle alive: when the last PLAYER leaves, the battle is dropped.
    #[test]
    fn the_battle_is_dropped_when_the_last_player_leaves_though_the_companions_are_still_there() {
        let t0 = Instant::now();
        let mut w = world_with_party(settings(SyntheticRules::default()), cloud_and_barret);
        let a = seat(&mut w, "Aria", t0);
        assert_eq!(w.request_encounter(a.entity, Some(0), t0), BattleReply::Done);
        assert_eq!(w.battle_count(), 1);
        assert!(w.leave(a.entity));
        assert_eq!(w.battle_count(), 0, "nobody is left to see it");
        assert!(logs_of(&mut w).iter().any(|l| l.contains("battle dropped: every player left")));
        w.advance_battles(ms(t0, 1_000)); // and nothing is left to step

        // With two players, one leaving leaves the companions and the other to finish.
        let mut w = world_with_party(settings(SyntheticRules::default()), cloud_and_barret);
        let (mut a, b) = (seat(&mut w, "A", t0), seat(&mut w, "B", t0));
        party_up(&mut w, &a, &b, t0);
        assert_eq!(w.request_encounter(a.entity, Some(1), t0), BattleReply::Done);
        let b_entity = b.entity;
        drop(b);
        assert!(w.leave(b_entity));
        assert_eq!(w.battle_count(), 1, "A is still there");
        play_out(&mut w, &mut [&mut a], &SyntheticRules::default(), t0);
        assert_eq!(w.battle_count(), 0);
    }

    // ---- pacing: the players go first (`companion::may_act`) ----

    /// The commands the server applied, in order, from what a seat was told: `(actor, command)`.
    fn applied(seat: &mut Seat) -> Vec<(ActorId, Command)> {
        seat.battle_msgs()
            .iter()
            .filter_map(|m| match m {
                ServerMsg::BattleEvent { command: Some(c), .. } => Some((c.actor, c.command)),
                _ => None,
            })
            .collect()
    }

    /// Advance the world's battles in 10 ms steps from `from` ms to `to` ms after `t0`.
    fn run(w: &mut World, t0: Instant, from: u64, to: u64) {
        let mut at = from;
        while at < to {
            at = (at + 10).min(to);
            w.advance_battles(ms(t0, at));
        }
    }

    /// Preemptive start, every gauge full: the companions hold theirs until the player has taken its turn, then follow at
    /// once, in actor order; the log says how long each held.
    #[test]
    fn the_players_go_first_and_the_companions_follow() {
        let t0 = Instant::now();
        let mut w = world_with_party(settings(SyntheticRules::default()), cloud_and_barret);
        let mut a = seat(&mut w, "Aria", t0);
        a.drain();
        assert_eq!(w.request_encounter(a.entity, Some(3), t0), BattleReply::Done);
        let (id, enemy) = (BattleId(1), ActorId(3));
        // Half a second of battle: the player has not chosen yet, so nobody has acted.
        run(&mut w, t0, 0, 500);
        assert!(applied(&mut a).is_empty(), "the companions wait for the player's turn");
        assert!(w.battle.as_ref().unwrap().battles.get(&id).unwrap().commands.iter().all(|&n| n == 0));
        // The player attacks; on the next step Cloud and Barret follow, in actor order.
        assert_eq!(w.battle_command(a.entity, id, ActorId(2), Command::Attack { target: enemy }, ms(t0, 500)), BattleReply::Done);
        run(&mut w, t0, 500, 510);
        let acted = applied(&mut a);
        assert_eq!(acted[0], (ActorId(2), Command::Attack { target: enemy }), "{acted:?}");
        assert_eq!(&acted[1..], &[(ActorId(0), Command::Attack { target: enemy }), (ActorId(1), Command::Attack { target: enemy })], "{acted:?}");
        let logs = logs_of(&mut w);
        let lines: Vec<&String> = logs.iter().filter(|l| l.starts_with("companion acted")).collect();
        assert_eq!(lines.len(), 2, "{logs:?}");
        for line in lines {
            let held: u64 = line.rsplit("held_ms=").next().and_then(|n| n.parse().ok()).expect("a held_ms");
            assert!((500..=520).contains(&held), "held for the player's half second: {line}");
        }
    }

    /// A player who does not choose holds the companions only for the turn wait, counted from its own full gauge; after
    /// that the fight goes on without it, and the companions do not wait for it again while its gauge stays full.
    #[test]
    fn an_idle_player_holds_the_companions_only_for_the_turn_wait() {
        let t0 = Instant::now();
        let pacing = companion::Pacing { turn_wait_ms: 300, max_wait_ms: 2_000 };
        let mut w = world_with_party(BattleSettings { companion_pacing: pacing, ..settings(SyntheticRules::default()) }, cloud_and_barret);
        let mut a = seat(&mut w, "Aria", t0);
        a.drain();
        assert_eq!(w.request_encounter(a.entity, Some(3), t0), BattleReply::Done);
        // At 1000 ticks a second the 300 ms turn wait is 300 ticks: nobody has acted just before it.
        run(&mut w, t0, 0, 290);
        assert!(applied(&mut a).is_empty());
        run(&mut w, t0, 290, 320);
        let acted = applied(&mut a);
        assert_eq!(acted.iter().map(|(actor, _)| *actor).collect::<Vec<_>>(), [ActorId(0), ActorId(1)], "the companions went without the idle player");
        // And they keep fighting without it: the battle ends while the player never acts.
        run(&mut w, t0, 320, 20_000);
        assert_eq!(w.battle_count(), 0, "the fight did not stall on the idle player");
        let logs = logs_of(&mut w);
        let ended = logs.iter().find(|l| l.starts_with("battle ended")).expect("an end line");
        assert!(ended.contains("outcome=Victory") && ended.contains(", 0] limits="), "the player gave nothing: {ended}");
    }

    /// The companions wait only for LIVING, CONNECTED players: one who left is not waited for, the other one still is.
    #[test]
    fn a_player_who_left_is_not_waited_for() {
        let t0 = Instant::now();
        let mut w = world_with_party(settings(SyntheticRules::default()), cloud_and_barret);
        let (mut a, b) = (seat(&mut w, "A", t0), seat(&mut w, "B", t0));
        party_up(&mut w, &a, &b, t0);
        a.drain();
        assert_eq!(w.request_encounter(a.entity, Some(3), t0), BattleReply::Done);
        let (id, enemy) = (BattleId(1), ActorId(4));
        run(&mut w, t0, 0, 100);
        assert!(applied(&mut a).is_empty(), "both players owe a turn");
        // B leaves; A still owes its turn, so nothing happens yet.
        let b_entity = b.entity;
        drop(b);
        assert!(w.leave(b_entity));
        run(&mut w, t0, 100, 200);
        assert!(applied(&mut a).is_empty(), "A is still waited for");
        // A acts: the companions follow at once, without waiting for B.
        assert_eq!(w.battle_command(a.entity, id, ActorId(2), Command::Attack { target: enemy }, ms(t0, 200)), BattleReply::Done);
        run(&mut w, t0, 200, 210);
        let actors: Vec<ActorId> = applied(&mut a).iter().map(|(actor, _)| *actor).collect();
        assert_eq!(actors, [ActorId(2), ActorId(0), ActorId(1)]);
    }
}
