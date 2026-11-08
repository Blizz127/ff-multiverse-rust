//! Authoritative world state for the single hosted area.
//!
//! Everything in here is synchronous and never touches a socket. Outbound messages are
//! pushed with non-blocking `try_send` into each connection's bounded queue. A client whose
//! queue is full (or already closed) is removed from the world and told to close through its
//! close flag, so one slow client can never stall the others or the tick loop.
//!
//! All time-dependent logic takes an explicit `Instant`, which keeps it deterministic under
//! test without sleeping.
//!
//! # Movement rules
//!
//! A `MoveIntent` is a straight chord from the entity's authoritative position to the position
//! the client claims. It is applied only if it passes, in this order:
//!
//! 1. **Finite**: no NaN/infinite position or facing. Facing is normalized into `(-pi, pi]`
//!    before it is stored and echoed to the other players.
//! 2. **Speed**: every player owns a travel budget, a token bucket measured in horizontal
//!    distance units on *server* time. It refills at `max_speed * SPEED_SLACK` units per second,
//!    holds at most [`MAX_ELAPSED`] worth of that, and starts empty when the player spawns. An
//!    accepted move costs its distance; a move costing more than the budget is `TooFast`
//!    (refused moves cost nothing). Because the budget is spent rather than re-granted per
//!    message, intents that arrive bunched together (TCP coalescing, jitter, a scheduling stall
//!    on either side) are judged against the time that really passed, and sending many small
//!    messages cannot beat `max_speed * SPEED_SLACK` on average: the only slack beyond that is a
//!    one-off float tolerance, never a per-message allowance.
//! 3. **Walkable**: adapters only validate a destination (`AreaRuntime::validate_move`), so the
//!    chord is walked in sub-steps of at most `max_speed / SUBSTEPS_PER_SEC` units (one 60 fps
//!    frame of travel), each validated from the previous accepted step. The move happens only if
//!    every sub-step is accepted; a hole or wall inside the chord leaves the entity where it was
//!    (`Blocked`). Known gap: a chord that clips the very corner of a reflex bend can be refused
//!    although the client's real path (validated frame by frame on its side) did not, which costs
//!    that client one snap-back.
//!
//! The speed rule measures horizontal distance only. Height is never taken from the client: the
//! adapter is shown the entity's own height and decides the new one, so vertical travel is
//! whatever the walkable surface allows for the horizontal travel that was paid for.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fmv_adapter_api::{AreaRuntime, MoveResult};
use fmv_core::{AnimState, AreaId, AvatarAppearance, EntityId, EntitySnapshot, Pos, SessionId};
use fmv_protocol::ServerMsg;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, watch};
use tracing::{debug, warn};

use crate::ServerError;

/// A message queued for one or more connections. Broadcasts share one allocation.
pub type Outbound = Arc<ServerMsg>;
pub type OutboundTx = mpsc::Sender<Outbound>;
/// Set to `true` to tell a connection task to stop (slow client, dead writer).
pub type CloseFlag = Arc<watch::Sender<bool>>;

/// A player may travel `SPEED_SLACK * max_speed` units per second on average.
pub const SPEED_SLACK: f64 = 1.5;
/// The travel budget never banks more than this much time worth of travel, however long the
/// client was silent (so a long pause cannot be turned into a teleport).
pub const MAX_ELAPSED: Duration = Duration::from_millis(500);
/// One-off tolerance for float noise, as a fraction of `max_speed` (in units). The budget may be
/// overdrawn by this much, but the debt is carried, never forgiven, so it does not add up.
pub const SPEED_TOLERANCE: f64 = 1e-5;
/// A chord is validated at least this many times per second of `max_speed` travel.
pub const SUBSTEPS_PER_SEC: f64 = 60.0;
/// Hard bound on adapter calls per `MoveIntent`. The budget cap already limits a chord to about
/// `SPEED_SLACK * MAX_ELAPSED * SUBSTEPS_PER_SEC = 45` sub-steps; this only guards the loop
/// against numeric surprises.
pub const MAX_SUBSTEPS: usize = 64;

/// Movement limits derived from the area's `max_speed`.
#[derive(Debug, Clone, Copy)]
struct MoveRules {
    /// Budget refill in distance units per second: `max_speed * SPEED_SLACK`.
    rate: f64,
    /// Most budget a player can hold: `rate * MAX_ELAPSED`.
    cap: f64,
    /// One-off float tolerance in distance units.
    tolerance: f64,
    /// Longest stretch of a chord handed to the adapter in one call.
    substep: f64,
}

impl MoveRules {
    fn new(max_speed: f32) -> Self {
        let v = f64::from(max_speed);
        let rate = v * SPEED_SLACK;
        Self { rate, cap: rate * MAX_ELAPSED.as_secs_f64(), tolerance: v * SPEED_TOLERANCE, substep: v / SUBSTEPS_PER_SEC }
    }
}

/// A player's travel budget (token bucket in distance units, on server time).
///
/// Invariant: `tokens >= -tolerance` (an accepted move never costs more than
/// `tokens + tolerance`) and `tokens <= cap` after a refill.
#[derive(Debug, Clone, Copy)]
struct MoveBudget {
    tokens: f64,
    /// Server time up to which the budget has been refilled.
    last: Instant,
}

impl MoveBudget {
    /// Empty at `now`: right after spawning, only the time since spawning has been earned.
    fn new(now: Instant) -> Self {
        Self { tokens: 0.0, last: now }
    }

    fn refill(&mut self, now: Instant, rules: &MoveRules) {
        // `None` means `now` is not after `last` (callers pass monotonic times); nothing to add.
        if let Some(dt) = now.checked_duration_since(self.last) {
            self.tokens = (self.tokens + dt.as_secs_f64() * rules.rate).min(rules.cap);
            self.last = now;
        }
    }

    /// Whether a move covering `dist` units can be paid for. A NaN distance never can. Turning
    /// or changing animation on the spot covers no distance, costs nothing and is always
    /// allowed, even when float noise leaves the carried debt a hair past the tolerance.
    fn can_afford(&self, dist: f64, rules: &MoveRules) -> bool {
        dist == 0.0 || matches!(dist.partial_cmp(&(self.tokens + rules.tolerance)), Some(Ordering::Less | Ordering::Equal))
    }
}

struct Player {
    session: SessionId,
    snapshot: EntitySnapshot,
    /// Travel budget for the speed rule.
    budget: MoveBudget,
    /// Changed since the last tick broadcast.
    dirty: bool,
    tx: OutboundTx,
    close: CloseFlag,
}

/// Everything the world needs to admit a player into the area.
pub struct NewPlayer {
    pub session: SessionId,
    pub entity: EntityId,
    pub avatar: AvatarAppearance,
    pub tx: OutboundTx,
    pub close: CloseFlag,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    /// NaN or infinite position/facing.
    NonFinite,
    /// Costs more than the player's travel budget holds (see the module docs).
    TooFast,
    /// The adapter refused the destination or a point on the way (off the walkable surface).
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MoveOutcome {
    Applied,
    /// The move was refused. `corrected` is the authoritative position to send back.
    Rejected { corrected: Pos, reason: Rejection },
    /// The entity is no longer in the world (dropped or disconnected).
    Gone,
}

pub struct World {
    area: Box<dyn AreaRuntime>,
    area_id: AreaId,
    spawn_points: Vec<Pos>,
    rules: MoveRules,
    next_spawn: usize,
    next_entity: u64,
    next_session: u64,
    tick: u64,
    /// Ordered by entity id so snapshots are deterministic.
    players: BTreeMap<EntityId, Player>,
}

impl World {
    pub fn new(area: Box<dyn AreaRuntime>) -> Result<Self, ServerError> {
        let area_id = area.info().id.clone();
        let spawn_points: Vec<Pos> = area.spawn_points().into_iter().filter(|p| p.is_finite()).collect();
        if spawn_points.is_empty() {
            return Err(ServerError::BadArea(area_id, "the area has no usable (finite) spawn points".into()));
        }
        let max_speed = area.max_speed();
        if !(max_speed.is_finite() && max_speed > 0.0) {
            return Err(ServerError::BadArea(area_id, format!("max_speed must be finite and positive (got {max_speed})")));
        }
        Ok(Self {
            area,
            area_id,
            spawn_points,
            rules: MoveRules::new(max_speed),
            next_spawn: 0,
            next_entity: 1,
            next_session: 1,
            tick: 0,
            players: BTreeMap::new(),
        })
    }

    pub fn current_tick(&self) -> u64 {
        self.tick
    }

    pub fn player_count(&self) -> usize {
        self.players.len()
    }

    /// Reserve the session and entity ids reported in `Welcome`. The entity only appears in the
    /// world once the client joins the area.
    pub fn allocate_ids(&mut self) -> (SessionId, EntityId) {
        let ids = (SessionId(self.next_session), EntityId(self.next_entity));
        self.next_session += 1;
        self.next_entity += 1;
        ids
    }

    /// Spawn the player (round-robin over the spawn points), queue the full `AreaSnapshot` to
    /// the joiner (always the first thing in its queue after the handshake replies) and
    /// `PlayerJoined` to everyone else. The movement budget starts empty. Returns the spawn
    /// position.
    pub fn join(&mut self, new: NewPlayer, now: Instant) -> Pos {
        let pos = self.spawn_points[self.next_spawn % self.spawn_points.len()];
        self.next_spawn = self.next_spawn.wrapping_add(1);
        let entity = new.entity;
        let snapshot = EntitySnapshot { id: entity, pos, facing: 0.0, anim: AnimState::Idle, avatar: new.avatar };
        self.players.insert(
            entity,
            Player {
                session: new.session,
                snapshot: snapshot.clone(),
                budget: MoveBudget::new(now),
                dirty: false,
                tx: new.tx,
                close: new.close,
            },
        );

        let area_snapshot = Arc::new(ServerMsg::AreaSnapshot {
            area: self.area_id.clone(),
            tick: self.tick,
            entities: self.players.values().map(|p| p.snapshot.clone()).collect(),
        });
        let joined = Arc::new(ServerMsg::PlayerJoined { snapshot });

        let mut failed = Vec::new();
        for (id, p) in &self.players {
            let msg = if *id == entity { &area_snapshot } else { &joined };
            if let Err(e) = p.tx.try_send(Arc::clone(msg)) {
                failed.push((*id, is_full(&e)));
            }
        }
        self.drop_failed(failed);
        pos
    }

    /// Remove a player and tell the others. Returns `false` if it was already gone.
    pub fn leave(&mut self, entity: EntityId) -> bool {
        if self.players.remove(&entity).is_none() {
            return false;
        }
        self.fan_out(Arc::new(ServerMsg::PlayerLeft { entity }));
        true
    }

    /// Validate and apply one `MoveIntent` (see the module docs for the rules). Order: finite
    /// check, speed budget, walking the chord through the adapter. Only an accepted move spends
    /// budget, and facing and anim are tracked only from accepted moves.
    pub fn apply_move(&mut self, entity: EntityId, to: Pos, facing: f32, anim: AnimState, now: Instant) -> MoveOutcome {
        let Some(p) = self.players.get_mut(&entity) else {
            return MoveOutcome::Gone;
        };
        let from = p.snapshot.pos;
        if !to.is_finite() || !facing.is_finite() {
            return MoveOutcome::Rejected { corrected: from, reason: Rejection::NonFinite };
        }
        let facing = normalize_facing(facing);

        p.budget.refill(now, &self.rules);
        let dist = dist_xy(from, to);
        if !p.budget.can_afford(dist, &self.rules) {
            return MoveOutcome::Rejected { corrected: from, reason: Rejection::TooFast };
        }

        match walk_chord(self.area.as_ref(), from, to, dist, &self.rules) {
            Chord::Clear(accepted) => {
                let changed = accepted != from || facing != p.snapshot.facing || anim != p.snapshot.anim;
                p.snapshot.pos = accepted;
                p.snapshot.facing = facing;
                p.snapshot.anim = anim;
                p.budget.tokens -= dist;
                p.dirty |= changed;
                MoveOutcome::Applied
            }
            Chord::Blocked { place } => {
                // `place` is where the entity already is, unless the adapter relocated it
                // (for example back onto the walkable surface).
                if place != from {
                    p.snapshot.pos = place;
                    p.dirty = true;
                }
                MoveOutcome::Rejected { corrected: place, reason: Rejection::Blocked }
            }
        }
    }

    /// Advance the tick counter and broadcast one `EntityUpdate` with every entity that changed
    /// since the previous tick (nothing is sent if nothing changed). Returns the new tick.
    pub fn advance_tick(&mut self) -> u64 {
        self.tick += 1;
        let changed: Vec<EntitySnapshot> = self
            .players
            .values_mut()
            .filter(|p| p.dirty)
            .map(|p| {
                p.dirty = false;
                p.snapshot.clone()
            })
            .collect();
        if !changed.is_empty() {
            self.fan_out(Arc::new(ServerMsg::EntityUpdate { tick: self.tick, entities: changed }));
        }
        self.tick
    }

    /// Queue `msg` for every joined player. Players whose queue is full or closed are dropped.
    fn fan_out(&mut self, msg: Outbound) {
        let mut failed = Vec::new();
        for (id, p) in &self.players {
            if let Err(e) = p.tx.try_send(Arc::clone(&msg)) {
                failed.push((*id, is_full(&e)));
            }
        }
        self.drop_failed(failed);
    }

    /// Remove players whose queue failed and announce their departure. Announcing can expose
    /// further failures, so keep going until the set is stable.
    fn drop_failed(&mut self, mut failed: Vec<(EntityId, bool)>) {
        while let Some((id, queue_full)) = failed.pop() {
            let Some(p) = self.players.remove(&id) else { continue };
            if queue_full {
                warn!(entity = id.0, session = p.session.0, "outbound queue full; disconnecting slow client");
            } else {
                debug!(entity = id.0, session = p.session.0, "outbound queue closed; removing player");
            }
            p.close.send_replace(true);
            let left = Arc::new(ServerMsg::PlayerLeft { entity: id });
            for (other_id, other) in &self.players {
                if let Err(e) = other.tx.try_send(Arc::clone(&left)) {
                    failed.push((*other_id, is_full(&e)));
                }
            }
        }
    }

    #[cfg(test)]
    fn snapshot_of(&self, entity: EntityId) -> Option<EntitySnapshot> {
        self.players.get(&entity).map(|p| p.snapshot.clone())
    }
}

fn is_full<T>(e: &TrySendError<T>) -> bool {
    matches!(e, TrySendError::Full(_))
}

/// How walking one chord through the adapter ended.
enum Chord {
    /// Every sub-step was accepted; the entity ends here (height snapped by the adapter).
    Clear(Pos),
    /// A sub-step was refused, so the move does not happen. The entity must be at `place`: where
    /// it was or, when the adapter relocates it on the very first step, where the adapter says.
    Blocked { place: Pos },
}

/// Walk the straight chord `from -> to` (horizontal length `dist`) through `validate_move` in
/// sub-steps of at most `rules.substep` units, each starting where the previous one was accepted.
/// The last sub-step ends exactly on `to`'s x/y. The client's z is never forwarded: the adapter
/// is always shown the entity's own height.
fn walk_chord(area: &dyn AreaRuntime, from: Pos, to: Pos, dist: f64, rules: &MoveRules) -> Chord {
    // `as usize` saturates and maps NaN to 0, so the clamp alone bounds the loop.
    let steps = ((dist / rules.substep).ceil() as usize).clamp(1, MAX_SUBSTEPS);
    let (fx, fy) = (f64::from(from.x), f64::from(from.y));
    let (dx, dy) = (f64::from(to.x) - fx, f64::from(to.y) - fy);
    let mut cur = from;
    for i in 1..=steps {
        let target = if i == steps {
            Pos::new(to.x, to.y, cur.z)
        } else {
            let t = i as f64 / steps as f64;
            Pos::new((fx + dx * t) as f32, (fy + dy * t) as f32, cur.z)
        };
        match area.validate_move(cur, target) {
            MoveResult::Accepted(next) if next.is_finite() => cur = next,
            MoveResult::Corrected(fixed) if i == 1 && fixed.is_finite() => return Chord::Blocked { place: fixed },
            // Refused further along (a hole or wall between the endpoints), or a misbehaving
            // adapter returned a non-finite position: keep the entity where it is.
            _ => return Chord::Blocked { place: from },
        }
    }
    Chord::Clear(cur)
}

/// Facing is echoed to every player and interpolated by them, so keep it a plain angle in
/// `(-pi, pi]` instead of whatever finite `f32` the client sent (an angle near `f32::MAX` would
/// turn into NaN in a peer's interpolation). Angles already in range are returned unchanged.
fn normalize_facing(facing: f32) -> f32 {
    use std::f32::consts::PI;
    if facing > -PI && facing <= PI {
        return facing;
    }
    let tau = std::f64::consts::TAU;
    let mut a = f64::from(facing).rem_euclid(tau); // [0, tau)
    if a > std::f64::consts::PI {
        a -= tau;
    }
    // Rounding to f32 can land exactly on -pi, which belongs to the other end of the interval.
    let wrapped = (a as f32).clamp(-PI, PI);
    if wrapped <= -PI {
        PI
    } else {
        wrapped
    }
}

/// Horizontal distance in f64 so even absurd finite inputs cannot overflow into NaN.
fn dist_xy(a: Pos, b: Pos) -> f64 {
    let dx = f64::from(a.x) - f64::from(b.x);
    let dy = f64::from(a.y) - f64::from(b.y);
    dx.hypot(dy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmv_adapter_api::{AreaInfo, Bounds, GameAdapter};
    use fmv_fixture::{FixtureAdapter, FIXTURE_AREA_KEY};
    use std::sync::Mutex;

    struct Seat {
        entity: EntityId,
        rx: mpsc::Receiver<Outbound>,
        close: watch::Receiver<bool>,
    }

    impl Seat {
        /// Everything queued so far.
        fn drain(&mut self) -> Vec<ServerMsg> {
            let mut out = Vec::new();
            while let Ok(m) = self.rx.try_recv() {
                out.push((*m).clone());
            }
            out
        }
    }

    fn world() -> World {
        World::new(FixtureAdapter::new().load_area(FIXTURE_AREA_KEY).unwrap()).unwrap()
    }

    fn seat(w: &mut World, name: &str, queue: usize, now: Instant) -> Seat {
        let (session, entity) = w.allocate_ids();
        let (tx, rx) = mpsc::channel(queue);
        let close = Arc::new(watch::channel(false).0);
        let close_rx = close.subscribe();
        let avatar = AvatarAppearance { display_name: name.into(), tint: [1, 2, 3], appearance_ref: None };
        w.join(NewPlayer { session, entity, avatar, tx, close }, now);
        Seat { entity, rx, close: close_rx }
    }

    fn ms(t0: Instant, n: u64) -> Instant {
        t0 + Duration::from_millis(n)
    }

    fn walk(w: &mut World, s: &Seat, x: f32, y: f32, now: Instant) -> MoveOutcome {
        w.apply_move(s.entity, Pos::new(x, y, 0.0), 0.5, AnimState::Walk, now)
    }

    /// Every call the adapter received: `(from, to)`.
    type Calls = Arc<Mutex<Vec<(Pos, Pos)>>>;

    /// Area stub without walls: accepts the first `accept_first` calls (snapping the height to
    /// 5), answers every later call with `Corrected(fixed)`, and records all calls. The only
    /// spawn point is the origin.
    struct Scripted {
        info: AreaInfo,
        speed: f32,
        accept_first: usize,
        fixed: Pos,
        calls: Calls,
    }

    impl AreaRuntime for Scripted {
        fn info(&self) -> &AreaInfo {
            &self.info
        }
        fn spawn_points(&self) -> Vec<Pos> {
            vec![Pos::default()]
        }
        fn validate_move(&self, from: Pos, to: Pos) -> MoveResult {
            let mut calls = self.calls.lock().unwrap();
            calls.push((from, to));
            if calls.len() <= self.accept_first {
                MoveResult::Accepted(Pos::new(to.x, to.y, 5.0))
            } else {
                MoveResult::Corrected(self.fixed)
            }
        }
        fn bounds(&self) -> Bounds {
            Bounds { min: Pos::default(), max: Pos::default() }
        }
        fn walk_triangles(&self) -> Vec<[Pos; 3]> {
            Vec::new()
        }
        fn max_speed(&self) -> f32 {
            self.speed
        }
    }

    fn scripted_world(speed: f32, accept_first: usize, fixed: Pos) -> (World, Calls) {
        let calls = Calls::default();
        let info = FixtureAdapter::new().list_areas().remove(0);
        let area = Scripted { info, speed, accept_first, fixed, calls: Arc::clone(&calls) };
        (World::new(Box::new(area)).unwrap(), calls)
    }

    /// Hop 700 ms after the previous call, so the budget is full again.
    fn hop(w: &mut World, s: &Seat, now: &mut Instant, x: f32, y: f32) -> MoveOutcome {
        *now += Duration::from_millis(700);
        walk(w, s, x, y, *now)
    }

    #[test]
    fn join_sends_snapshot_to_joiner_and_joined_to_others_with_round_robin_spawns() {
        let t0 = Instant::now();
        let mut w = world();
        let mut a = seat(&mut w, "A", 16, t0);
        match &a.drain()[..] {
            [ServerMsg::AreaSnapshot { entities, area, .. }] => {
                assert_eq!(area.key, FIXTURE_AREA_KEY);
                assert_eq!(entities.len(), 1);
                assert_eq!(entities[0].id, a.entity);
                assert_eq!(entities[0].pos, Pos::new(-100.0, 0.0, 0.0));
            }
            other => panic!("unexpected: {other:?}"),
        }
        let mut b = seat(&mut w, "B", 16, t0);
        match &b.drain()[..] {
            [ServerMsg::AreaSnapshot { entities, .. }] => {
                assert_eq!(entities.len(), 2);
                assert_eq!(w.snapshot_of(b.entity).unwrap().pos, Pos::new(100.0, 0.0, 0.0));
            }
            other => panic!("unexpected: {other:?}"),
        }
        match &a.drain()[..] {
            [ServerMsg::PlayerJoined { snapshot }] => {
                assert_eq!(snapshot.id, b.entity);
                assert_eq!(snapshot.avatar.display_name, "B");
            }
            other => panic!("unexpected: {other:?}"),
        }
        // Spawn points wrap around (the fixture has four).
        let _c = seat(&mut w, "C", 16, t0);
        let _d = seat(&mut w, "D", 16, t0);
        let e = seat(&mut w, "E", 16, t0);
        assert_eq!(w.snapshot_of(e.entity).unwrap().pos, Pos::new(-100.0, 0.0, 0.0));
        assert_eq!(w.player_count(), 5);
    }

    #[test]
    fn accepted_move_is_broadcast_once_and_idle_ticks_send_nothing() {
        let t0 = Instant::now();
        let mut w = world();
        let mut a = seat(&mut w, "A", 16, t0);
        let mut b = seat(&mut w, "B", 16, t0);
        a.drain();
        b.drain();

        // No change yet: ticking sends nothing.
        w.advance_tick();
        assert!(a.drain().is_empty() && b.drain().is_empty());

        // 100 ms after spawn the budget holds 600 * 1.5 * 0.1 = 90 units; moving 80 is fine.
        // The z value the client sent is replaced by the adapter's snapped height.
        let out = w.apply_move(a.entity, Pos::new(-20.0, 0.0, 9.0), 1.25, AnimState::Walk, ms(t0, 100));
        assert_eq!(out, MoveOutcome::Applied);
        assert_eq!(w.advance_tick(), 2);
        let mover = a.entity;
        // The update goes to every joined client, the mover included.
        for seat in [&mut a, &mut b] {
            match &seat.drain()[..] {
                [ServerMsg::EntityUpdate { tick, entities }] => {
                    assert_eq!(*tick, 2);
                    assert_eq!(entities.len(), 1);
                    assert_eq!(entities[0].id, mover);
                    assert_eq!(entities[0].pos, Pos::new(-20.0, 0.0, 0.0));
                    assert_eq!(entities[0].facing, 1.25);
                    assert_eq!(entities[0].anim, AnimState::Walk);
                }
                other => panic!("unexpected: {other:?}"),
            }
        }

        // Re-sending identical state is accepted but is not a change: nothing is broadcast.
        assert_eq!(w.apply_move(a.entity, Pos::new(-20.0, 0.0, 0.0), 1.25, AnimState::Walk, ms(t0, 200)), MoveOutcome::Applied);
        w.advance_tick();
        assert!(a.drain().is_empty() && b.drain().is_empty());
    }

    #[test]
    fn speed_budget_accrues_on_server_time_and_is_capped_at_half_a_second() {
        let t0 = Instant::now();
        let mut w = world();
        let a = seat(&mut w, "A", 16, t0);

        // The budget starts empty: right after spawning nothing has been earned yet.
        let out = walk(&mut w, &a, -50.0, 0.0, ms(t0, 0));
        assert_eq!(out, MoveOutcome::Rejected { corrected: Pos::new(-100.0, 0.0, 0.0), reason: Rejection::TooFast });
        // 100 ms earn 600 * 1.5 * 0.1 = 90 units. Moving 80 spends them and leaves 10.
        assert_eq!(walk(&mut w, &a, -20.0, 0.0, ms(t0, 100)), MoveOutcome::Applied);
        // 10 ms later 10 + 9 = 19 units are available, so a 250-unit jump is refused and the last
        // accepted position is reported.
        let out = walk(&mut w, &a, 230.0, 0.0, ms(t0, 110));
        assert_eq!(out, MoveOutcome::Rejected { corrected: Pos::new(-20.0, 0.0, 0.0), reason: Rejection::TooFast });
        // The refusal spent nothing: at 500 ms the budget is 19 + 900 * 0.39 = 370, so 320 fit.
        assert_eq!(walk(&mut w, &a, 300.0, 0.0, ms(t0, 500)), MoveOutcome::Applied);
        // However long the client was silent, the budget holds at most half a second of travel
        // at 1.5x max speed: 450 units.
        let far = ms(t0, 60_000);
        let out = walk(&mut w, &a, -156.0, 0.0, far); // distance 456
        assert_eq!(out, MoveOutcome::Rejected { corrected: Pos::new(300.0, 0.0, 0.0), reason: Rejection::TooFast });
        assert_eq!(walk(&mut w, &a, -150.0, 0.0, far), MoveOutcome::Applied); // distance 450
        // ...and that spent all of it: nothing more in the same instant...
        let out = walk(&mut w, &a, -140.0, 0.0, far);
        assert!(matches!(out, MoveOutcome::Rejected { reason: Rejection::TooFast, .. }));
        // ...except turning on the spot, which covers no distance and so costs nothing.
        let out = w.apply_move(a.entity, Pos::new(-150.0, 0.0, 0.0), 2.5, AnimState::Idle, far);
        assert_eq!(out, MoveOutcome::Applied);
        assert_eq!(w.snapshot_of(a.entity).unwrap().facing, 2.5);
    }

    #[test]
    fn affordability_is_nan_safe_and_turning_on_the_spot_is_free() {
        let rules = MoveRules::new(600.0);
        let mut budget = MoveBudget::new(Instant::now());
        // Float noise left the carried debt a hair past the tolerance: a move that covers no
        // distance (a facing or animation change) must still go through, anything else must not.
        budget.tokens = -rules.tolerance - 1e-12;
        assert!(budget.can_afford(0.0, &rules));
        assert!(!budget.can_afford(1e-6, &rules));
        // A NaN distance can never be paid for.
        assert!(!budget.can_afford(f64::NAN, &rules));
        // Exactly the budget plus the one-off tolerance is affordable; a hair more is not.
        budget.tokens = 10.0;
        assert!(budget.can_afford(10.0, &rules));
        assert!(budget.can_afford(10.0 + rules.tolerance, &rules));
        assert!(!budget.can_afford(10.0 + rules.tolerance * 2.0, &rules));
    }

    #[test]
    fn bunched_intents_are_judged_against_the_time_that_really_passed() {
        let t0 = Instant::now();
        let mut w = world();
        let a = seat(&mut w, "A", 16, t0);
        // After a pause the budget is full (450 units). A client that kept walking at 18 units per
        // 50 ms (the headless bot's pace) through a stall of the network or of the server has all
        // those intents handled in the same instant. Each is judged against the budget it earned
        // over the stall, not against the ~1 unit that "time since the previous intent" would give.
        let at = ms(t0, 1_000);
        for i in 1..=25 {
            let x = -100.0 + 18.0 * i as f32;
            assert_eq!(walk(&mut w, &a, x, 0.0, at), MoveOutcome::Applied, "step {i}");
        }
        // 25 steps cost exactly the 450 units the budget can hold: the 26th is too much...
        let out = walk(&mut w, &a, -100.0 + 18.0 * 26.0, 0.0, at);
        assert!(matches!(out, MoveOutcome::Rejected { reason: Rejection::TooFast, .. }));
        // ...and only the time that passes afterwards pays for more: 50 ms earn 45 units, which
        // covers two more 18-unit steps but not a third.
        let later = ms(t0, 1_050);
        assert_eq!(walk(&mut w, &a, 368.0, 0.0, later), MoveOutcome::Applied);
        assert_eq!(walk(&mut w, &a, 386.0, 0.0, later), MoveOutcome::Applied);
        let out = walk(&mut w, &a, 404.0, 0.0, later);
        assert!(matches!(out, MoveOutcome::Rejected { reason: Rejection::TooFast, .. }));
    }

    #[test]
    fn overspeed_does_not_grow_with_the_message_rate() {
        // A client at the 120 msgs/s rate limit asks for slightly more than the budget refills
        // per message. With a per-message epsilon its speed would be 1.5 * max_speed + 120 u/s
        // (1.7x at 600 u/s, 3.5x at 60 u/s). With a spent budget it is capped at 1.5 * max_speed
        // whatever the message rate or the adapter's scale.
        for max_speed in [600.0_f32, 60.0] {
            let t0 = Instant::now();
            let (mut w, _calls) = scripted_world(max_speed, usize::MAX, Pos::default());
            let a = seat(&mut w, "A", 16, t0);
            let mut x = 0.0_f32;
            let mut elapsed = Duration::ZERO;
            for _ in 0..1200 {
                elapsed += Duration::from_nanos(8_333_333); // 120 messages per second for 10 s
                let target = x + 8.5;
                if walk(&mut w, &a, target, 0.0, t0 + elapsed) == MoveOutcome::Applied {
                    x = target;
                }
            }
            let allowed = f64::from(max_speed) * SPEED_SLACK * elapsed.as_secs_f64();
            let travelled = f64::from(x);
            assert!(travelled <= allowed + f64::from(max_speed) * SPEED_TOLERANCE, "{max_speed}: travelled {travelled} of {allowed} allowed");
            assert!(travelled >= allowed * 0.5, "{max_speed}: only {travelled} of {allowed}; the test would prove nothing");
        }
    }

    #[test]
    fn a_chord_across_a_hole_is_refused_even_when_both_ends_are_walkable() {
        let t0 = Instant::now();
        let mut w = world();
        let mut a = seat(&mut w, "A", 64, t0);
        let mut b = seat(&mut w, "B", 64, t0);
        let mut now = t0;
        // Legal hops next to the notch (x > 400, y > 0 is not walkable).
        assert_eq!(hop(&mut w, &a, &mut now, 300.0, 0.0), MoveOutcome::Applied);
        assert_eq!(hop(&mut w, &a, &mut now, 300.0, 250.0), MoveOutcome::Applied);
        w.advance_tick();
        a.drain();
        b.drain();

        // (500, -100) is walkable and only 403 units away, well inside the speed budget, but the
        // straight line there crosses the notch (for example at (420, 40)). Checking only the
        // destination would let a client cut across any hole narrower than the budget.
        let out = hop(&mut w, &a, &mut now, 500.0, -100.0);
        assert_eq!(out, MoveOutcome::Rejected { corrected: Pos::new(300.0, 250.0, 0.0), reason: Rejection::Blocked });
        w.advance_tick();
        assert!(a.drain().is_empty() && b.drain().is_empty(), "a refused chord changes and broadcasts nothing");
        assert_eq!(w.snapshot_of(a.entity).unwrap().pos, Pos::new(300.0, 250.0, 0.0));

        // Walking around the notch is fine.
        assert_eq!(hop(&mut w, &a, &mut now, 300.0, -100.0), MoveOutcome::Applied);
        assert_eq!(hop(&mut w, &a, &mut now, 500.0, -100.0), MoveOutcome::Applied);
        assert_eq!(w.snapshot_of(a.entity).unwrap().pos, Pos::new(500.0, -100.0, 0.0));
    }

    #[test]
    fn a_chord_is_validated_in_short_steps_each_starting_where_the_last_was_accepted() {
        let t0 = Instant::now();
        let (mut w, calls) = scripted_world(600.0, usize::MAX, Pos::default());
        let a = seat(&mut w, "A", 16, t0);
        // 300 units after a full budget; the client claims a height of 99, which must never reach
        // the adapter (the adapter snaps every accepted step to a height of 5).
        let out = w.apply_move(a.entity, Pos::new(240.0, 180.0, 99.0), 0.0, AnimState::Walk, ms(t0, 1_000));
        assert_eq!(out, MoveOutcome::Applied);
        let calls = calls.lock().unwrap();
        // max_speed / 60 = 10 units per step.
        assert_eq!(calls.len(), 30);
        let mut expected_from = Pos::default();
        for (i, (from, to)) in calls.iter().enumerate() {
            assert_eq!(*from, expected_from, "step {i} starts where the previous one was accepted");
            assert!(from.dist_xy(*to) <= 10.0 + 1e-3, "step {i} is {} units long", from.dist_xy(*to));
            assert_eq!(to.z, from.z, "the client's height is never forwarded");
            expected_from = Pos::new(to.x, to.y, 5.0);
        }
        assert_eq!(calls.last().unwrap().1.x, 240.0, "the last step ends exactly on the requested x");
        assert_eq!(calls.last().unwrap().1.y, 180.0, "the last step ends exactly on the requested y");
        drop(calls);
        assert_eq!(w.snapshot_of(a.entity).unwrap().pos, Pos::new(240.0, 180.0, 5.0));
    }

    #[test]
    fn a_refusal_later_in_the_chord_leaves_the_entity_and_its_budget_alone() {
        let t0 = Instant::now();
        // The adapter accepts three steps, then claims to want the entity at (7, 7).
        let (mut w, calls) = scripted_world(600.0, 3, Pos::new(7.0, 7.0, 0.0));
        let mut a = seat(&mut w, "A", 16, t0);
        a.drain();
        let at = ms(t0, 1_000);
        let out = walk(&mut w, &a, 100.0, 0.0, at);
        assert_eq!(out, MoveOutcome::Rejected { corrected: Pos::default(), reason: Rejection::Blocked });
        assert_eq!(calls.lock().unwrap().len(), 4, "the walk stops at the first refusal");
        assert_eq!(w.snapshot_of(a.entity).unwrap().pos, Pos::default(), "the adapter's correction is ignored mid-chord");
        // The budget refilled to its 450-unit cap while the client was quiet and none was spent.
        assert_eq!(w.players[&a.entity].budget.tokens, 450.0, "a refused move costs nothing");
        w.advance_tick();
        assert!(a.drain().is_empty());
    }

    #[test]
    fn the_adapter_may_relocate_the_entity_when_it_refuses_the_first_step() {
        let t0 = Instant::now();
        // For example the entity no longer stands on the walkable surface.
        let (mut w, _calls) = scripted_world(600.0, 0, Pos::new(7.0, 7.0, 0.0));
        let mut a = seat(&mut w, "A", 16, t0);
        a.drain();
        let out = walk(&mut w, &a, 50.0, 0.0, ms(t0, 1_000));
        assert_eq!(out, MoveOutcome::Rejected { corrected: Pos::new(7.0, 7.0, 0.0), reason: Rejection::Blocked });
        assert_eq!(w.snapshot_of(a.entity).unwrap().pos, Pos::new(7.0, 7.0, 0.0));
        w.advance_tick();
        assert!(matches!(&a.drain()[..], [ServerMsg::EntityUpdate { entities, .. }] if entities[0].pos == Pos::new(7.0, 7.0, 0.0)));
    }

    #[test]
    fn facing_is_normalized_into_one_turn_and_in_range_values_are_untouched() {
        use std::f32::consts::{PI, TAU};
        // In range: bit for bit (the integration tests compare 1.25 and 2.0 exactly).
        for f in [0.0_f32, -0.0, 1.25, 2.0, -3.0, PI, PI - 1e-6, -(PI - 1e-6)] {
            assert_eq!(normalize_facing(f).to_bits(), f.to_bits(), "{f}");
        }
        // The interval is (-pi, pi]: -pi is the same direction as +pi, so it moves to the other end.
        // (`f32` PI is a hair above the real pi, so its mirror image lands a hair below it.)
        let flipped = normalize_facing(-PI);
        assert!(flipped > 3.0 && flipped <= PI, "{flipped}");
        // Out of range: the same direction, inside (-pi, pi].
        for f in [4.0_f32, -4.0, 7.0, -7.0, 100.0, -100.0, 1.0e6, -1.0e6, 3.0e38, -3.0e38, f32::MAX, f32::MIN, -3.0 * PI, 3.0 * PI] {
            let n = normalize_facing(f);
            assert!(n.is_finite() && n > -PI && n <= PI, "{f} -> {n}");
            if f.abs() <= 1.0e6 {
                let turns = (f64::from(f) - f64::from(n)) / std::f64::consts::TAU;
                assert!((turns - turns.round()).abs() < 1e-5, "{f} -> {n} is a different direction");
            }
        }
        assert!((normalize_facing(7.0) - (7.0 - TAU)).abs() < 1e-5);
        assert!((normalize_facing(-7.0) - (-7.0 + TAU)).abs() < 1e-5);

        // What the other players are told is the normalized value.
        let t0 = Instant::now();
        let mut w = world();
        let a = seat(&mut w, "A", 16, t0);
        for f in [3.0e38_f32, -3.0e38] {
            let out = w.apply_move(a.entity, Pos::new(-100.0, 0.0, 0.0), f, AnimState::Walk, ms(t0, 1_000));
            assert_eq!(out, MoveOutcome::Applied);
            let stored = w.snapshot_of(a.entity).unwrap().facing;
            assert!(stored.is_finite() && stored > -PI && stored <= PI, "{f} -> {stored}");
        }
    }

    #[test]
    fn off_mesh_move_is_corrected_without_any_broadcast() {
        let t0 = Instant::now();
        let mut w = world();
        let mut a = seat(&mut w, "A", 16, t0);
        let mut b = seat(&mut w, "B", 16, t0);
        a.drain();
        b.drain();
        // Well within the speed budget, but y = 350 is outside the plaza (max 300).
        let out = walk(&mut w, &a, -100.0, 350.0, ms(t0, 1_000));
        assert_eq!(out, MoveOutcome::Rejected { corrected: Pos::new(-100.0, 0.0, 0.0), reason: Rejection::Blocked });
        w.advance_tick();
        assert!(a.drain().is_empty() && b.drain().is_empty());
        assert_eq!(w.snapshot_of(a.entity).unwrap().pos, Pos::new(-100.0, 0.0, 0.0));
        assert_eq!(w.snapshot_of(a.entity).unwrap().anim, AnimState::Idle, "rejected moves do not change anim");
    }

    #[test]
    fn non_finite_values_are_rejected() {
        let t0 = Instant::now();
        let mut w = world();
        let a = seat(&mut w, "A", 16, t0);
        let spawn = Pos::new(-100.0, 0.0, 0.0);
        let at = ms(t0, 1_000);
        for (pos, facing) in [
            (Pos::new(f32::NAN, 0.0, 0.0), 0.0),
            (Pos::new(0.0, f32::INFINITY, 0.0), 0.0),
            (Pos::new(0.0, 0.0, f32::NEG_INFINITY), 0.0),
            (Pos::new(-100.0, 0.0, 0.0), f32::NAN),
            (Pos::new(-100.0, 0.0, 0.0), f32::INFINITY),
        ] {
            assert_eq!(
                w.apply_move(a.entity, pos, facing, AnimState::Idle, at),
                MoveOutcome::Rejected { corrected: spawn, reason: Rejection::NonFinite }
            );
        }
        // Finite but absurd values must not overflow into a bogus "accepted".
        let out = walk(&mut w, &a, f32::MAX, -f32::MAX, at);
        assert!(matches!(out, MoveOutcome::Rejected { reason: Rejection::TooFast, .. }));
    }

    #[test]
    fn moves_of_unknown_entities_are_reported_gone() {
        let mut w = world();
        let out = w.apply_move(EntityId(99), Pos::default(), 0.0, AnimState::Idle, Instant::now());
        assert_eq!(out, MoveOutcome::Gone);
    }

    #[test]
    fn leave_announces_once() {
        let t0 = Instant::now();
        let mut w = world();
        let a = seat(&mut w, "A", 16, t0);
        let mut b = seat(&mut w, "B", 16, t0);
        b.drain();
        assert!(w.leave(a.entity));
        assert!(!w.leave(a.entity), "second leave is a no-op");
        assert!(matches!(&b.drain()[..], [ServerMsg::PlayerLeft { entity }] if *entity == a.entity));
        assert_eq!(w.player_count(), 1);
    }

    #[test]
    fn slow_client_is_dropped_without_affecting_others() {
        let t0 = Instant::now();
        let mut w = world();
        // `slow` never drains its tiny queue; `fast` drains after every step.
        let slow = seat(&mut w, "slow", 3, t0);
        let mut fast = seat(&mut w, "fast", 64, t0);
        fast.drain();
        // slow's queue holds: AreaSnapshot, PlayerJoined(fast) -> one more slot.
        // `fast` is the second joiner, so it spawns at the second spawn point, (100, 0).
        let mut x = 100.0;
        let mut now = ms(t0, 0);
        let mut seen = Vec::new();
        for _ in 0..4 {
            now += Duration::from_millis(100);
            x += 10.0;
            assert_eq!(walk(&mut w, &fast, x, 0.0, now), MoveOutcome::Applied);
            w.advance_tick();
            seen.extend(fast.drain());
        }
        assert!(*slow.close.borrow(), "slow client must be told to close");
        assert_eq!(w.player_count(), 1, "slow client is removed from the world");
        // The fast client kept receiving, and was told the slow one left.
        assert!(seen.iter().any(|m| matches!(m, ServerMsg::PlayerLeft { entity } if *entity == slow.entity)));
        assert_eq!(seen.iter().filter(|m| matches!(m, ServerMsg::EntityUpdate { .. })).count(), 4);
        assert!(!*fast.close.borrow());
    }

    #[test]
    fn closed_queue_removes_the_player() {
        let t0 = Instant::now();
        let mut w = world();
        let gone = seat(&mut w, "gone", 8, t0);
        let mut stay = seat(&mut w, "stay", 8, t0);
        stay.drain();
        drop(gone.rx); // the writer task died
        let mover_out = walk(&mut w, &stay, 0.0, 0.0, ms(t0, 1_000));
        assert_eq!(mover_out, MoveOutcome::Applied);
        w.advance_tick();
        assert_eq!(w.player_count(), 1);
        assert!(*gone.close.borrow());
        assert!(stay.drain().iter().any(|m| matches!(m, ServerMsg::PlayerLeft { entity } if *entity == gone.entity)));
    }

    #[test]
    fn world_rejects_unusable_areas() {
        use fmv_adapter_api::{AreaInfo, Bounds};

        struct Bare {
            info: AreaInfo,
            spawns: Vec<Pos>,
            speed: f32,
        }
        impl AreaRuntime for Bare {
            fn info(&self) -> &AreaInfo {
                &self.info
            }
            fn spawn_points(&self) -> Vec<Pos> {
                self.spawns.clone()
            }
            fn validate_move(&self, _from: Pos, to: Pos) -> MoveResult {
                MoveResult::Accepted(to)
            }
            fn bounds(&self) -> Bounds {
                Bounds { min: Pos::default(), max: Pos::default() }
            }
            fn walk_triangles(&self) -> Vec<[Pos; 3]> {
                Vec::new()
            }
            fn max_speed(&self) -> f32 {
                self.speed
            }
        }
        let info = FixtureAdapter::new().list_areas().remove(0);
        let mk = |spawns: Vec<Pos>, speed: f32| -> Box<dyn AreaRuntime> { Box::new(Bare { info: info.clone(), spawns, speed }) };
        assert!(World::new(mk(vec![Pos::default()], 10.0)).is_ok());
        assert!(World::new(mk(vec![], 10.0)).is_err());
        assert!(World::new(mk(vec![Pos::new(f32::NAN, 0.0, 0.0)], 10.0)).is_err());
        assert!(World::new(mk(vec![Pos::default()], 0.0)).is_err());
        assert!(World::new(mk(vec![Pos::default()], f32::NAN)).is_err());
    }
}
