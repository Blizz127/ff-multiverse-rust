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
//! * A battle starts for the requester's whole party (the members that are in the area and in no
//!   battle yet) or for the requester alone. Member `i` (ascending entity id, the requester
//!   included) fights as `ActorId(i)`, as the `i`-th character of the fixed order Cloud, Barret,
//!   Tifa, Aerith, Red XIII, Yuffie, Cait Sith, Vincent, Cid: accounts have no character of their
//!   own yet (that needs per-account progression), so this is the interim rule.
//! * A player in a battle is FROZEN: its `MoveIntent`s are ignored (not applied, not answered),
//!   so nobody else sees it move, and it takes no random-encounter steps.
//! * When the battle ends every member is back in the field where it stood. Its entity is marked
//!   changed, so the next tick's `EntityUpdate` tells it (and everyone) where that is.
//! * A member that disconnects stays in the engine as an uncontrolled character; a battle with no
//!   connected member left is dropped.
//! * Rewards are logged and sent in `BattleEnd`, but NOT stored: an account's progression blob is
//!   also its story phase key (`accounts::login` builds `PhaseKey(progression.blob)`), so writing
//!   battle results into it would move the player to another instance.
//!
//! # Time
//!
//! Like everything in the world, stepping takes an explicit `Instant`; the engine consumes the
//! whole milliseconds since its last step (the engine's own accumulator turns them into ticks).
//!
//! # Random encounters
//!
//! In an area whose `AreaRuntime::encounters()` is a non-empty table, every `encounter_step_units`
//! of horizontal distance a player moves is one roll: it triggers with probability `rate / 1024`
//! and then picks a formation by the table's weights. APPROXIMATION: this is not FF7's own step
//! counter, only a rate knob over the same table. By default a step is the area's `max_speed`
//! (one roll per second of walking at full speed, so the table's rate 128 means roughly one battle
//! per eight seconds of full-speed walking); `random_encounters: false` switches the rolls off.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fmv_adapter_api::EncounterTable;
use fmv_core::EntityId;
use fmv_ff7_battle::rng::Rng;
use fmv_ff7_battle::rules::{ActorId, BattleState, Command, CommandError, Outcome, MAX_STEP_MS};
use fmv_protocol::{split_rewards, AppliedCommand, BattleId, BattleMember, BattleRejection, BattleSetup, ServerMsg, MAX_BATTLE_PARTY};
use tracing::{debug, info, warn};

use super::{is_full, Outbound, World};
use crate::config::BattleSettings;

/// The characters party members fight as, by their place in the battle (see the module docs).
const CHARACTER_ORDER: [u8; 9] = [0, 1, 2, 3, 4, 5, 6, 7, 8];
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

/// A member of a running battle.
struct Member {
    info: BattleMember,
    /// False once its player left the world (its character stays in the engine, uncontrolled).
    connected: bool,
}

/// One running battle.
struct Battle {
    state: BattleState,
    /// `members[i]` fights as `ActorId(i)`.
    members: Vec<Member>,
    /// The engine has consumed time up to here.
    clock: Instant,
}

impl Battle {
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

    fn connected(&self) -> Vec<EntityId> {
        self.members.iter().filter(|m| m.connected).map(|m| m.info.entity).collect()
    }

    fn actor_of(&self, entity: EntityId) -> Option<ActorId> {
        let i = self.members.iter().position(|m| m.info.entity == entity)?;
        u8::try_from(i).ok().map(ActorId)
    }
}

/// Everything the world keeps for battles. Absent (`None` in `World`) on a server that cannot run
/// them.
pub(super) struct BattleWorld {
    settings: BattleSettings,
    debug_encounters: bool,
    /// The area's random encounters, when it has any and they are on.
    encounters: Option<EncounterTable>,
    /// Horizontal distance between two random-encounter rolls.
    step: f64,
    battles: BTreeMap<BattleId, Battle>,
    next_id: u64,
    /// Seeds, random-encounter rolls and weighted formation picks.
    rng: Rng,
}

impl BattleWorld {
    pub(super) fn new(settings: BattleSettings, debug_encounters: bool, encounters: Option<EncounterTable>, max_speed: f32) -> Self {
        let rng = Rng::new(settings.fixed_seed.unwrap_or_else(fresh_seed));
        let step = f64::from(settings.encounter_step_units.unwrap_or(max_speed)).max(1.0);
        let encounters = encounters.filter(|t| settings.random_encounters && !t.is_empty());
        Self { settings, debug_encounters, encounters, step, battles: BTreeMap::new(), next_id: 1, rng }
    }

    /// The seed of the next battle.
    fn next_seed(&mut self) -> u64 {
        self.settings.fixed_seed.unwrap_or_else(|| (u64::from(self.rng.next_u32()) << 32) | u64::from(self.rng.next_u32()))
    }

    /// A weighted pick from the area's table, if it has one.
    fn pick_from_table(&mut self) -> Option<u16> {
        let total = self.encounters.as_ref()?.total_weight();
        let roll = self.rng.below(total);
        self.encounters.as_ref()?.pick(roll)
    }

    /// The formation for a debug request that names none: the operator's choice, else the area's
    /// table, else the rules' default.
    fn default_formation(&mut self) -> Option<u16> {
        self.settings
            .debug_formation
            .or_else(|| self.pick_from_table())
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

/// The wire reason for an engine refusal.
fn rejection_of(e: CommandError) -> BattleRejection {
    match e {
        CommandError::BattleOver => BattleRejection::BattleOver,
        CommandError::UnknownActor(_) | CommandError::NotControllable(_) => BattleRejection::NotYourActor,
        CommandError::ActorDead(_) => BattleRejection::ActorDead,
        CommandError::GaugeNotFull(_) => BattleRejection::GaugeNotFull,
        CommandError::UnknownTarget(_) | CommandError::InvalidTarget(_) => BattleRejection::InvalidTarget,
        CommandError::TargetDead(_) => BattleRejection::TargetDead,
    }
}

impl World {
    /// Switch battles on (`settings` of `Some`) or leave them unsupported (`None`, every request is
    /// refused). The area's encounter table is read here, once.
    pub fn configure_battle(&mut self, settings: Option<BattleSettings>, debug_encounters: bool) {
        let table = self.area.encounters();
        let max_speed = self.area.max_speed();
        self.battle = settings.map(|s| BattleWorld::new(s, debug_encounters, table, max_speed));
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
        if !self.players.contains_key(&entity) {
            return BattleReply::Gone;
        }
        let Some(bw) = self.battle.as_mut() else {
            return refused(None, BattleRejection::Unsupported);
        };
        if !bw.debug_encounters {
            return refused(None, BattleRejection::DebugDisabled);
        }
        let Some(formation) = formation.or_else(|| bw.default_formation()) else {
            return refused(None, BattleRejection::UnknownFormation);
        };
        match self.begin_battle(entity, formation, now) {
            Ok(_) => BattleReply::Done,
            Err(reason) => refused(None, reason),
        }
    }

    /// Start a battle with `formation` for `requester`'s party (or just `requester`).
    fn begin_battle(&mut self, requester: EntityId, formation: u16, now: Instant) -> Result<BattleId, BattleRejection> {
        let Some(bw) = self.battle.as_mut() else {
            return Err(BattleRejection::Unsupported);
        };
        let Some(player) = self.players.get(&requester) else {
            return Err(BattleRejection::NotInBattle);
        };
        if player.battle.is_some() {
            return Err(BattleRejection::AlreadyInBattle);
        }
        let info = bw.settings.rules.formation(formation).ok_or(BattleRejection::UnknownFormation)?;

        // Who fights: the requester and the rest of its party that are here and free.
        let mut others: BTreeSet<EntityId> = BTreeSet::new();
        if let Some(party) = self.parties.party_of(player.account) {
            for member in &party.members {
                let Some(&entity) = self.by_account.get(member) else { continue };
                if entity != requester && self.players.get(&entity).is_some_and(|p| p.battle.is_none()) {
                    others.insert(entity);
                }
            }
        }
        let mut entities: Vec<EntityId> = std::iter::once(requester).chain(others.into_iter().take(MAX_BATTLE_PARTY - 1)).collect();
        entities.sort();

        let party: Vec<BattleMember> = entities
            .iter()
            .enumerate()
            .filter_map(|(i, &entity)| {
                let account = self.players.get(&entity)?.account;
                let character = CHARACTER_ORDER[i % CHARACTER_ORDER.len()];
                let level = bw.settings.level.unwrap_or_else(|| bw.settings.rules.starting_level(character));
                Some(BattleMember { entity, account, character, level, weapon: None, armor: None })
            })
            .collect();

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
                warn!(formation, error = %e, "cannot build the battle from the server's own data");
                return Err(BattleRejection::UnknownFormation);
            }
        };
        let id = BattleId(bw.next_id);
        bw.next_id += 1;
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
            Battle { state, members: party.iter().cloned().map(|info| Member { info, connected: true }).collect(), clock: now },
        );
        for entity in &entities {
            if let Some(p) = self.players.get_mut(entity) {
                p.battle = Some(id);
            }
        }
        info!(
            battle = id.0,
            formation,
            members = entities.len(),
            enemies = info.enemies,
            preemptive = info.preemptive,
            back_attack = info.back_attack,
            seed,
            "battle started"
        );
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
        match battle.state.submit_command(actor, command) {
            Ok(events) => {
                let msg = ServerMsg::BattleEvent {
                    battle_id,
                    tick: battle.state.tick(),
                    events,
                    command: Some(AppliedCommand { actor, command }),
                };
                let (ended, recipients) = (battle.state.outcome().is_some(), battle.connected());
                self.fan_to(&recipients, Arc::new(msg));
                if ended {
                    self.finish_battle(battle_id);
                }
                BattleReply::Done
            }
            Err(e) => refused(Some(battle_id), rejection_of(e)),
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
        let (tick, ended, recipients) = (battle.state.tick(), battle.state.outcome().is_some(), battle.connected());
        if !events.is_empty() {
            self.fan_to(&recipients, Arc::new(ServerMsg::BattleEvent { battle_id: id, tick, events, command: None }));
        }
        if ended {
            self.finish_battle(id);
        }
    }

    /// End a battle whose engine has an outcome: tell the members, free them, log the rewards.
    fn finish_battle(&mut self, id: BattleId) {
        let Some(bw) = self.battle.as_mut() else { return };
        let Some(outcome) = bw.battles.get(&id).and_then(|b| b.state.outcome()) else { return };
        let Some(battle) = bw.battles.remove(&id) else { return };
        let roster: Vec<(fmv_core::AccountId, bool)> = battle
            .members
            .iter()
            .enumerate()
            .map(|(i, m)| (m.info.account, u8::try_from(i).ok().and_then(|i| battle.state.actor(ActorId(i))).is_some_and(|c| c.is_alive())))
            .collect();
        let rewards = if outcome == Outcome::Victory { split_rewards(battle.state.rewards(), &roster) } else { Vec::new() };
        info!(battle = id.0, ?outcome, ticks = battle.state.tick(), members = battle.members.len(), "battle ended");
        for r in &rewards {
            let delivered = battle.members.iter().any(|m| m.info.account == r.account && m.connected);
            info!(
                battle = id.0,
                account = r.account.0,
                exp = r.exp,
                gil = r.gil,
                ap = r.ap,
                delivered,
                "battle reward (logged and sent only: progression is not written, it is also the story phase)"
            );
        }
        let recipients = battle.connected();
        // Back in the field: free everyone first, and mark them changed so the next `EntityUpdate`
        // says where they stand.
        for m in &battle.members {
            if let Some(p) = self.players.get_mut(&m.info.entity) {
                if p.battle == Some(id) {
                    p.battle = None;
                    p.dirty = true;
                    p.walked = 0.0;
                }
            }
        }
        self.fan_to(&recipients, Arc::new(ServerMsg::BattleEnd { battle_id: id, outcome, rewards }));
    }

    /// A player that was in battle `id` left the world: its character stays in the engine,
    /// uncontrolled, and the battle is dropped when nobody is left to see it.
    pub(super) fn member_gone(&mut self, id: BattleId, entity: EntityId) {
        let Some(bw) = self.battle.as_mut() else { return };
        let Some(battle) = bw.battles.get_mut(&id) else { return };
        for m in &mut battle.members {
            if m.info.entity == entity {
                m.connected = false;
            }
        }
        if battle.members.iter().all(|m| !m.connected) {
            bw.battles.remove(&id);
            info!(battle = id.0, "battle dropped: every member left");
        }
    }

    /// Count `dist` units of walking towards a random-encounter roll and start a battle when one
    /// triggers (see the module docs). Does nothing without battle support or an encounter table.
    pub(super) fn roll_encounter(&mut self, entity: EntityId, dist: f64, now: Instant) {
        let triggered = {
            let Some(bw) = self.battle.as_mut() else { return };
            let Some(rate) = bw.encounters.as_ref().map(|t| u32::from(t.rate)) else { return };
            let step = bw.step;
            let Some(p) = self.players.get_mut(&entity) else { return };
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
                bw.pick_from_table()
            } else {
                None
            }
        };
        if let Some(formation) = triggered {
            if let Err(reason) = self.begin_battle(entity, formation, now) {
                debug!(entity = entity.0, formation, ?reason, "a random encounter could not start");
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
    use crate::world::{MoveOutcome, NewPlayer};
    use fmv_adapter_api::{AreaRuntime, EncounterEntry};
    use fmv_core::{AccountId, AnimState, AvatarAppearance, PhaseKey, Pos};
    use fmv_fixture::{FixtureAdapter, FIXTURE_AREA_KEY};
    use fmv_adapter_api::{AreaInfo, Bounds, GameAdapter, MoveResult};
    use fmv_protocol::battle::BattleEvent;
    use fmv_protocol::{BattleReplay, SyntheticRules};
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

    fn world_with(settings: Option<BattleSettings>, debug: bool) -> World {
        let mut w = World::new(FixtureAdapter::new().load_area(FIXTURE_AREA_KEY).unwrap()).unwrap();
        w.configure_battle(settings, debug);
        w
    }

    fn seat(w: &mut World, name: &str, now: Instant) -> Seat {
        let (session, entity) = w.allocate_ids();
        let (tx, rx) = mpsc::channel(4096);
        let close = Arc::new(watch::channel(false).0);
        let avatar = AvatarAppearance { display_name: name.into(), tint: [1, 2, 3], appearance_ref: None };
        let account = AccountId(entity.0);
        w.join(NewPlayer { session, entity, account, avatar, phase: PhaseKey::default(), tx, close }, now).expect("joins");
        Seat { entity, account, rx, log: Vec::new() }
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
                    let mine = ActorId(replay.party().iter().position(|m| m.entity == seat.entity).unwrap() as u8);
                    if state.outcome().is_none() && state.ready_actors().contains(&mine) {
                        let target = state.enemy_ids().into_iter().find(|e| state.actor(*e).is_some_and(|c| c.is_alive())).unwrap();
                        let id = seat_battle_id(&seat.log);
                        match w.battle_command(seat.entity, id, mine, Command::Attack { target }, now) {
                            BattleReply::Done => {}
                            BattleReply::Refused { reason, .. } => panic!("a ready actor's attack was refused: {reason}"),
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
                assert_eq!((party[0].entity, party[0].account, party[0].character, party[0].level), (a.entity, a.account, 0, 5));
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
        let (mut a, mut b) = party_of_two(&mut w, t0);
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
                let who: Vec<(EntityId, u8)> = party.iter().map(|m| (m.entity, m.character)).collect();
                assert_eq!(who, vec![(a.entity, 0), (b.entity, 1)], "ascending entity ids fight as Cloud, Barret");
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
}
