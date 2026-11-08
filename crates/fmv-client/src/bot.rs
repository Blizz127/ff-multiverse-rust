//! Scripted headless bot movement.
//!
//! The bot never teleports: each step moves toward a moving target at `BOT_SPEED_FACTOR *
//! max_speed` at most, and the step is validated with `AreaRuntime::validate_move`. When a step
//! is refused the pattern shrinks, so the bot converges on a path that stays on the walkable
//! surface whatever the area's shape or the spawn point. If the adapter answers `Corrected` with
//! a position other than the bot's own (it stands off the walkable surface), the bot is placed
//! there, exactly as the server will place it.

use crate::cli::BotKind;
use crate::nav::plan_path;
use crate::net::PartyCommand;
use crate::party::PartyMirror;
use fmv_adapter_api::{AreaKey, AreaRuntime, Bounds, Exit, MoveResult};
use fmv_core::{EntityId, Pos};
use fmv_protocol::InviteId;
use std::collections::{BTreeSet, VecDeque};

/// Bots walk at this fraction of the area's `max_speed` (the server enforces the maximum).
pub const BOT_SPEED_FACTOR: f32 = 0.6;
/// A pattern is abandoned once it shrinks below this fraction of its initial size.
const MIN_SIZE_FRACTION: f32 = 0.02;
const SHRINK: f32 = 0.75;

#[derive(Debug, Clone)]
pub struct Bot {
    kind: BotKind,
    center: Pos,
    speed: f32,
    /// Circle radius / line half-length in area units.
    size: f32,
    min_size: f32,
    /// Circle phase (radians) or line direction angle.
    phase: f32,
    /// Line only: which end is the current target.
    toward_far_end: bool,
    /// The exits of the area the bot walks in (read from the area the first time it steps in it):
    /// a pattern bot never walks across one, because that would take it, and any party it leads,
    /// to another area. Only `--travel` bots do (see [`TravelBot`]).
    exits: Vec<Exit>,
    exits_of: Option<AreaKey>,
}

/// Outcome of one bot step.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BotStep {
    pub pos: Pos,
    /// Direction of travel in radians, if the bot moved.
    pub facing: Option<f32>,
    pub moved: bool,
}

impl Bot {
    pub fn new(kind: BotKind, spawn: Pos, bounds: Bounds, max_speed: f32) -> Self {
        let w = (bounds.max.x - bounds.min.x).abs();
        let h = (bounds.max.y - bounds.min.y).abs();
        let extent = w.min(h);
        let extent = if extent.is_finite() && extent > 0.0 {
            extent
        } else {
            100.0
        };
        let size = (extent * 0.25).max(1.0);
        let speed = if max_speed.is_finite() && max_speed > 0.0 {
            max_speed * BOT_SPEED_FACTOR
        } else {
            0.0
        };
        Self {
            kind,
            center: spawn,
            speed,
            size,
            min_size: size * MIN_SIZE_FRACTION,
            phase: 0.0,
            toward_far_end: true,
            exits: Vec::new(),
            exits_of: None,
        }
    }

    /// Remember the exits of `area` (once per area).
    fn sync_exits(&mut self, area: &dyn AreaRuntime) {
        if self.exits_of.as_ref() != Some(&area.info().id.key) {
            self.exits = area.exits();
            self.exits_of = Some(area.info().id.key.clone());
        }
    }

    /// True once the pattern shrank to nothing (the bot stands still from then on).
    pub fn is_stuck(&self) -> bool {
        self.size < self.min_size
    }

    fn target(&self) -> Pos {
        let c = self.center;
        match self.kind {
            BotKind::Idle => c,
            BotKind::Circle => Pos::new(
                c.x + self.size * self.phase.cos(),
                c.y + self.size * self.phase.sin(),
                c.z,
            ),
            BotKind::Line => {
                let sign = if self.toward_far_end { 1.0 } else { -1.0 };
                Pos::new(
                    c.x + sign * self.size * self.phase.cos(),
                    c.y + sign * self.size * self.phase.sin(),
                    c.z,
                )
            }
        }
    }

    /// Advance the bot by `dt` seconds from `cur`.
    pub fn step(&mut self, area: &dyn AreaRuntime, cur: Pos, dt: f32) -> BotStep {
        let still = BotStep {
            pos: cur,
            facing: None,
            moved: false,
        };
        if self.kind == BotKind::Idle || self.is_stuck() || !(dt.is_finite() && dt > 0.0) || self.speed <= 0.0 {
            return still;
        }
        if self.kind == BotKind::Circle {
            // Angular speed keeps the target's linear speed equal to the bot's speed.
            self.phase = (self.phase + self.speed / self.size * dt) % std::f32::consts::TAU;
        }
        let target = self.target();
        let (dx, dy) = (target.x - cur.x, target.y - cur.y);
        let dist = dx.hypot(dy);
        let max_step = self.speed * dt;
        if self.kind == BotKind::Line && dist <= max_step {
            self.toward_far_end = !self.toward_far_end;
        }
        let (sx, sy) = if dist <= max_step {
            (dx, dy)
        } else {
            (dx / dist * max_step, dy / dist * max_step)
        };
        if sx == 0.0 && sy == 0.0 {
            return still;
        }
        let want = Pos::new(cur.x + sx, cur.y + sy, cur.z);
        // A step across an exit line is refused like a step off the walkable surface: the pattern
        // shrinks until it stays on this side of every exit.
        self.sync_exits(area);
        if self.exits.iter().any(|e| e.is_crossed_by(cur, want)) {
            self.size *= SHRINK;
            return still;
        }
        match area.validate_move(cur, want) {
            MoveResult::Accepted(p) if p.is_finite() => BotStep {
                pos: p,
                facing: Some(sy.atan2(sx)),
                moved: true,
            },
            // Refused AND told to stand elsewhere (`cur` is off the walkable surface or at the
            // wrong height): stand there, as the server will place us. The pattern leaves the
            // walkable surface as well, so it shrinks like for any refusal.
            MoveResult::Corrected(p) if p.is_finite() && p != cur => {
                self.size *= SHRINK;
                BotStep {
                    pos: p,
                    facing: None,
                    moved: false,
                }
            }
            _ => {
                // Refused: the pattern leaves the walkable surface, so make it smaller.
                self.size *= SHRINK;
                still
            }
        }
    }
}


// ---------------------------------------------------------------------------------------------
// Travel behaviour
// ---------------------------------------------------------------------------------------------

/// A bot that is to form a party first (`--party-invite-first-peer`, `--party-accept`) holds still
/// until it has two members, but at most this many seconds.
pub const PARTY_WAIT_S: f64 = 12.0;
/// A bot that has arrived in an area it must leave again waits at least this long (the players
/// there show up in the first updates)...
pub const ARRIVAL_LINGER_S: f64 = 1.0;
/// ...and until it has seen another player there, but at most this many seconds.
pub const PEER_WAIT_S: f64 = 8.0;
/// How far past the middle of an exit line the bot aims, so that a step that does not end exactly on
/// the line ends beyond it.
const OVERSHOOT: f32 = 30.0;
/// Headings tried in a step, as degrees away from the straight line to the goal, best first.
const FANOUT_DEG: [f32; 11] = [0.0, 15.0, -15.0, 30.0, -30.0, 45.0, -45.0, 60.0, -60.0, 90.0, -90.0];

/// What the world looks like to a bot deciding whether to walk (see [`TravelBot::may_walk`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TravelGate {
    /// Seconds on the session clock.
    pub now: f64,
    /// Members of our party (0: in none).
    pub party_size: usize,
    /// Other players seen in the current area so far.
    pub peers_seen: usize,
}

/// What `--travel` does: walk through the exits to a list of areas, in order. A pure state machine
/// over the area's own exits and `validate_move`; nothing here touches the network or the clock.
///
/// It plans a way to the middle of the exit that leads to the next area (aiming a little past it, so
/// the crossing really happens) with [`plan_path`]: a few long straight legs that keep clear of the
/// walls, which is what the server's chord check agrees to (a bot that slid along walls step by step
/// made chords it refused, over and over). Should the planner find no way, or a leg turn out blocked,
/// it falls back to the greedy fan: each step takes the heading nearest the goal's that is walkable
/// and gets closer. It never crosses an exit other than the one it wants. The bot gives up (and says
/// so) when no exit of the current area leads to the next area, or when it makes no headway for
/// [`STUCK_LIMIT`] steps.
#[derive(Debug)]
pub struct TravelBot {
    /// The areas still to go to; the front one is next.
    route: VecDeque<String>,
    /// Hold still until the party has two members (see [`PARTY_WAIT_S`]).
    wait_for_party: bool,
    /// When the bot arrived in the area it is in now (`None`: it started here).
    arrived_at: Option<f64>,
    /// Steps in a row that made no headway.
    stuck: u32,
    /// Why the bot gave up, if it did.
    gave_up: Option<String>,
    /// The planned legs to the exit of the area it is in.
    plan: Option<Plan>,
    /// Steps to go before planning is tried again after it found nothing or a leg was blocked.
    plan_cooldown: u32,
    /// Plans that came to nothing in this area (the greedy fan takes over for good after a few).
    failed_plans: u32,
}

/// The planned way to an exit.
#[derive(Debug, Clone)]
struct Plan {
    /// The area it is for, and the area it leads to.
    area: AreaKey,
    target: String,
    /// The first waypoint is where the bot was; `next` is the one it walks to.
    waypoints: Vec<Pos>,
    next: usize,
}

/// Steps to wait before planning again after a plan failed (a second at 100 steps per second).
const PLAN_COOLDOWN_STEPS: u32 = 100;
/// Plans that may fail in one area before the bot stops planning there.
const MAX_FAILED_PLANS: u32 = 4;

/// Steps without headway after which a [`TravelBot`] gives up (at 100 steps per second: 5 s).
pub const STUCK_LIMIT: u32 = 500;

impl TravelBot {
    /// `route`: the areas to travel to, in order (empty: the bot does not travel).
    pub fn new(route: Vec<String>, wait_for_party: bool) -> Self {
        Self {
            route: route.into(),
            wait_for_party,
            arrived_at: None,
            stuck: 0,
            gave_up: None,
            plan: None,
            plan_cooldown: 0,
            failed_plans: 0,
        }
    }

    /// There is somewhere left to go (and the bot has not given up).
    pub fn is_active(&self) -> bool {
        !self.route.is_empty() && self.gave_up.is_none()
    }

    /// The area it is heading for.
    pub fn next_area(&self) -> Option<&str> {
        self.route.front().map(String::as_str)
    }

    /// Why the bot stopped trying, if it did.
    pub fn gave_up(&self) -> Option<&str> {
        self.gave_up.as_deref()
    }

    /// The session entered `area` at `now`. If it is the next stop it is crossed off. Returns
    /// whether it was (arriving anywhere else, say by an exit the bot did not mean, leaves the route
    /// as it was: the bot goes on from wherever it is).
    pub fn arrived(&mut self, area: &str, now: f64) -> bool {
        self.stuck = 0;
        self.plan = None;
        self.plan_cooldown = 0;
        self.failed_plans = 0;
        self.arrived_at = Some(now);
        if self.route.front().is_some_and(|next| next.eq_ignore_ascii_case(area)) {
            self.route.pop_front();
            return true;
        }
        false
    }

    /// May the bot walk right now? It does not while it waits for its party, nor while it lingers
    /// in an area to be seen by the others before it leaves for the next one.
    pub fn may_walk(&self, gate: TravelGate) -> bool {
        if !self.is_active() {
            return false;
        }
        if self.wait_for_party && gate.party_size < 2 && gate.now < PARTY_WAIT_S {
            return false;
        }
        if let Some(at) = self.arrived_at {
            let waited = gate.now - at;
            if waited < ARRIVAL_LINGER_S || (gate.peers_seen == 0 && waited < PEER_WAIT_S) {
                return false;
            }
        }
        true
    }

    /// Advance the bot by `dt` seconds from `cur` at `speed` units per second.
    pub fn step(&mut self, area: &dyn AreaRuntime, cur: Pos, dt: f32, speed: f32) -> BotStep {
        let still = BotStep { pos: cur, facing: None, moved: false };
        let Some(next) = self.route.front().filter(|_| self.gave_up.is_none()).cloned() else { return still };
        if !(dt.is_finite() && dt > 0.0 && speed.is_finite() && speed > 0.0) {
            return still;
        }
        let exits = area.exits();
        let Some(goal_exit) = exits.iter().find(|e| e.target.eq_ignore_ascii_case(&next)) else {
            self.gave_up = Some(format!("no exit of {} leads to {next}", area.info().id));
            return still;
        };
        let mid = goal_exit.midpoint();
        let (gx, gy) = (mid.x - cur.x, mid.y - cur.y);
        let dist = gx.hypot(gy);
        if !dist.is_finite() {
            return still;
        }
        // Aim a little past the middle of the line, along the way we are coming.
        let (ux, uy) = if dist > 1e-3 { (gx / dist, gy / dist) } else { (1.0, 0.0) };
        let aim = Pos::new(mid.x + ux * OVERSHOOT, mid.y + uy * OVERSHOOT, mid.z);
        let heading = (aim.y - cur.y).atan2(aim.x - cur.x);
        let len = speed * dt;

        if let Some(step) = self.follow_plan(area, &exits, goal_exit, cur, (aim, mid), len, &next) {
            return step;
        }

        let mut best: Option<(f32, Pos, f32)> = None; // (distance to the aim afterwards, position, heading)
        for deg in FANOUT_DEG {
            let h = heading + deg.to_radians();
            let want = Pos::new(cur.x + h.cos() * len, cur.y + h.sin() * len, cur.z);
            // Other exits are not for us; the wanted one is crossed on purpose.
            if exits.iter().any(|e| !std::ptr::eq(e, goal_exit) && e.is_crossed_by(cur, want)) {
                continue;
            }
            if let MoveResult::Accepted(p) = area.validate_move(cur, want) {
                if p.is_finite() {
                    let left = p.dist_xy(aim);
                    if best.is_none_or(|(b, ..)| left < b - 1e-3) {
                        best = Some((left, p, h));
                    }
                }
            }
        }
        match best {
            Some((left, pos, h)) if left < cur.dist_xy(aim) - 1e-3 => {
                self.stuck = 0;
                BotStep { pos, facing: Some(h), moved: true }
            }
            _ => {
                // No heading gets us closer: wait a moment (the party or the world may change) and
                // give up if it lasts.
                self.stuck += 1;
                if self.stuck >= STUCK_LIMIT {
                    self.gave_up = Some(format!("stuck on the way to the exit to {next} at ({:.0}, {:.0})", cur.x, cur.y));
                }
                still
            }
        }
    }

    /// One step along the planned legs to the exit (planning them first when there is no plan for
    /// this area and target). `None`: no plan could be made or the next leg is blocked; the caller
    /// takes a greedy step instead and the plan is made again after a pause.
    #[allow(clippy::too_many_arguments)]
    fn follow_plan(
        &mut self,
        area: &dyn AreaRuntime,
        exits: &[Exit],
        goal_exit: &Exit,
        cur: Pos,
        (aim, mid): (Pos, Pos),
        len: f32,
        next: &str,
    ) -> Option<BotStep> {
        if self.failed_plans >= MAX_FAILED_PLANS {
            return None;
        }
        let key = area.info().id.key.clone();
        if self.plan.as_ref().is_none_or(|p| p.area != key || !p.target.eq_ignore_ascii_case(next)) {
            self.plan = None;
            if self.plan_cooldown > 0 {
                self.plan_cooldown -= 1;
                return None;
            }
            match plan_path(area, cur, aim).or_else(|| plan_path(area, cur, mid)) {
                Some(waypoints) => self.plan = Some(Plan { area: key, target: next.to_string(), waypoints, next: 1 }),
                None => {
                    self.failed_plans += 1;
                    self.plan_cooldown = PLAN_COOLDOWN_STEPS;
                    return None;
                }
            }
        }
        let plan = self.plan.as_mut()?;
        while plan.waypoints.get(plan.next).is_some_and(|wp| cur.dist_xy(*wp) < 1.0) {
            plan.next += 1;
        }
        let Some(wp) = plan.waypoints.get(plan.next).copied() else {
            // Every waypoint is behind us and the exit was not crossed: the plan has run out.
            self.plan = None;
            self.failed_plans += 1;
            return None;
        };
        let dist = cur.dist_xy(wp);
        let (ux, uy) = ((wp.x - cur.x) / dist, (wp.y - cur.y) / dist);
        let step_len = len.min(dist);
        let want = Pos::new(cur.x + ux * step_len, cur.y + uy * step_len, cur.z);
        // Other exits are not for us; the wanted one is crossed on purpose.
        let blocked = exits.iter().any(|e| !std::ptr::eq(e, goal_exit) && e.is_crossed_by(cur, want));
        match area.validate_move(cur, want) {
            MoveResult::Accepted(p) if p.is_finite() && !blocked => {
                if step_len >= dist {
                    plan.next += 1;
                }
                self.stuck = 0;
                Some(BotStep { pos: p, facing: Some(uy.atan2(ux)), moved: true })
            }
            _ => {
                // The leg is not walkable after all (or crosses another exit): plan again later.
                self.plan = None;
                self.failed_plans += 1;
                self.plan_cooldown = PLAN_COOLDOWN_STEPS;
                None
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Party behaviour
// ---------------------------------------------------------------------------------------------

/// A refused invite is sent again at most this many times (after the first send)...
pub const MAX_INVITE_RETRIES: u32 = 5;
/// ...and never more often than this many seconds apart.
pub const INVITE_RETRY_S: f64 = 1.0;
/// Invite ids remembered as accepted (bounded against a hostile server).
const MAX_ACCEPTED: usize = 4096;

/// What the bot does about parties (`--party-invite-first-peer`, `--party-accept`). A pure state
/// machine: [`PartyBot::step`] looks at the party mirror and the visible players and says which
/// requests to send; nothing here touches the network or the clock.
#[derive(Debug)]
pub struct PartyBot {
    invite_first_peer: bool,
    accept: bool,
    invite: InviteDriver,
    accepted: BTreeSet<InviteId>,
}

/// The state of "invite the first peer".
#[derive(Debug, Default)]
struct InviteDriver {
    /// `PartyInvite`s sent so far (the first one and its retries).
    sends: u32,
    last_sent: Option<f64>,
    /// The mirror's refusal count when the last invite went out: a refusal is news only if the
    /// count has grown since.
    refusals_at_send: u64,
    /// Nothing more to do: an invite was seen, or a party formed, or the retries ran out.
    done: bool,
}

impl PartyBot {
    /// `invite_first_peer`: invite the visible player with the lowest entity id as soon as there
    /// is one. `accept`: accept every invite addressed to this account, once per invite.
    pub fn new(invite_first_peer: bool, accept: bool) -> Self {
        Self { invite_first_peer, accept, invite: InviteDriver::default(), accepted: BTreeSet::new() }
    }

    /// The requests to send now. `now` is seconds on the session clock; `visible` the entities of
    /// the remote players that can be seen right now.
    ///
    /// * Invite: the first request goes to the lowest visible entity id. If the server refuses it
    ///   (`PartyRejected`) while this account is in no party, it is sent again, never more often
    ///   than every [`INVITE_RETRY_S`] and at most [`MAX_INVITE_RETRIES`] times, to whoever has the
    ///   lowest id then. It stops for good once an invite of ours was announced or a party formed.
    /// * Accept: every pending invite addressed to this account is accepted exactly once.
    pub fn step(&mut self, now: f64, mirror: &PartyMirror, visible: &[EntityId]) -> Vec<PartyCommand> {
        let mut out = Vec::new();
        if self.invite_first_peer {
            if let Some(target) = self.invite.step(now, mirror, visible) {
                out.push(PartyCommand::Invite(target));
            }
        }
        if self.accept {
            for invite in mirror.invites_to_me() {
                if self.accepted.len() < MAX_ACCEPTED && self.accepted.insert(invite.id) {
                    out.push(PartyCommand::Accept(invite.id));
                }
            }
        }
        out
    }
}

impl InviteDriver {
    /// The entity to invite now, if an invite is due.
    fn step(&mut self, now: f64, mirror: &PartyMirror, visible: &[EntityId]) -> Option<EntityId> {
        if self.done {
            return None;
        }
        if mirror.party_size() > 0 || mirror.outgoing_invite_seen() {
            self.done = true;
            return None;
        }
        let target = visible.iter().min().copied()?;
        if let Some(last) = self.last_sent {
            if mirror.rejection_count() <= self.refusals_at_send {
                return None; // no news: the answer (an announced invite or a refusal) is pending
            }
            if self.sends > MAX_INVITE_RETRIES {
                self.done = true;
                return None;
            }
            if now - last < INVITE_RETRY_S {
                return None; // refused, but too soon to try again
            }
        }
        self.sends += 1;
        self.last_sent = Some(now);
        self.refusals_at_send = mirror.rejection_count();
        Some(target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmv_adapter_api::GameAdapter;
    use fmv_fixture::{FixtureAdapter, FIXTURE_AREA_KEY};

    /// Independent on-mesh check (does not use `validate_move`).
    fn on_mesh(tris: &[[Pos; 3]], p: Pos) -> bool {
        let side = |a: Pos, b: Pos, c: Pos| (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
        tris.iter().any(|t| {
            let (d1, d2, d3) = (side(t[0], t[1], p), side(t[1], t[2], p), side(t[2], t[0], p));
            let neg = d1 < -1e-3 || d2 < -1e-3 || d3 < -1e-3;
            let pos = d1 > 1e-3 || d2 > 1e-3 || d3 > 1e-3;
            !(neg && pos)
        })
    }

    struct Run {
        max_step: f32,
        path_len: f32,
        min: (f32, f32),
        max: (f32, f32),
        stuck: bool,
    }

    /// Simulate `secs` seconds at 50 ms ticks, asserting the invariants on every single step.
    fn simulate(kind: BotKind, spawn: Pos, secs: f32) -> Run {
        let adapter = FixtureAdapter::new();
        let area = adapter.load_area(FIXTURE_AREA_KEY).unwrap();
        let tris = area.walk_triangles();
        let mut bot = Bot::new(kind, spawn, area.bounds(), area.max_speed());
        let dt = 0.05;
        let limit = area.max_speed() * BOT_SPEED_FACTOR * dt * 1.001;
        let mut cur = spawn;
        let mut run = Run {
            max_step: 0.0,
            path_len: 0.0,
            min: (cur.x, cur.y),
            max: (cur.x, cur.y),
            stuck: false,
        };
        for _ in 0..(secs / dt) as usize {
            let s = bot.step(area.as_ref(), cur, dt);
            let d = cur.dist_xy(s.pos);
            assert!(d <= limit, "{kind:?}: step {d} exceeds {limit} (0.6 * max_speed * dt)");
            assert!(d < area.max_speed() * dt, "{kind:?}: step must stay below max_speed");
            assert!(
                on_mesh(&tris, s.pos),
                "{kind:?}: left the mesh at ({}, {}) from spawn {spawn:?}",
                s.pos.x,
                s.pos.y
            );
            assert_eq!(s.moved, s.facing.is_some());
            run.max_step = run.max_step.max(d);
            run.path_len += d;
            cur = s.pos;
            run.min = (run.min.0.min(cur.x), run.min.1.min(cur.y));
            run.max = (run.max.0.max(cur.x), run.max.1.max(cur.y));
        }
        run.stuck = bot.is_stuck();
        run
    }

    #[test]
    fn circle_bot_stays_on_mesh_below_max_speed_for_every_fixture_spawn() {
        let adapter = FixtureAdapter::new();
        let area = adapter.load_area(FIXTURE_AREA_KEY).unwrap();
        for spawn in area.spawn_points() {
            let r = simulate(BotKind::Circle, spawn, 30.0);
            assert!(!r.stuck, "spawn {spawn:?}");
            assert!(
                r.path_len > 500.0,
                "bot should actually travel, got {} from {spawn:?}",
                r.path_len
            );
            assert!(
                r.max.0 - r.min.0 > 50.0 && r.max.1 - r.min.1 > 50.0,
                "a circle spans both axes: {:?}..{:?}",
                r.min,
                r.max
            );
            // 0.6 * 600 * 0.05 = 18 units per tick; never faster.
            assert!(r.max_step <= 18.0 * 1.001, "max step {}", r.max_step);
        }
    }

    #[test]
    fn line_bot_stays_on_mesh_and_covers_ground() {
        let adapter = FixtureAdapter::new();
        let area = adapter.load_area(FIXTURE_AREA_KEY).unwrap();
        for spawn in area.spawn_points() {
            let r = simulate(BotKind::Line, spawn, 30.0);
            assert!(!r.stuck, "spawn {spawn:?}");
            assert!(r.path_len > 500.0, "got {} from {spawn:?}", r.path_len);
        }
    }

    #[test]
    fn bots_adapt_near_walls_and_corners() {
        // Spawns pressed against edges force the pattern to shrink; the path must stay on the mesh.
        for spawn in [
            Pos::new(695.0, -5.0, 0.0),   // far corner of the arm
            Pos::new(-399.0, 299.0, 0.0), // corner of the main quad
            Pos::new(0.0, 299.5, 0.0),    // along the top wall
            Pos::new(420.0, -295.0, 0.0), // bottom edge, in the arm
        ] {
            for kind in [BotKind::Circle, BotKind::Line] {
                let r = simulate(kind, spawn, 20.0);
                assert!(r.max_step <= 18.0 * 1.001);
            }
        }
    }

    #[test]
    fn idle_bot_never_moves() {
        let r = simulate(BotKind::Idle, Pos::new(-100.0, 0.0, 0.0), 5.0);
        assert_eq!(r.path_len, 0.0);
    }

    #[test]
    fn first_step_moves_immediately_and_reports_facing() {
        let adapter = FixtureAdapter::new();
        let area = adapter.load_area(FIXTURE_AREA_KEY).unwrap();
        let mut bot = Bot::new(
            BotKind::Circle,
            Pos::new(-100.0, 0.0, 0.0),
            area.bounds(),
            area.max_speed(),
        );
        let s = bot.step(area.as_ref(), Pos::new(-100.0, 0.0, 0.0), 0.05);
        assert!(s.moved && s.facing.is_some());
    }

    #[test]
    fn degenerate_inputs_do_not_panic() {
        let adapter = FixtureAdapter::new();
        let area = adapter.load_area(FIXTURE_AREA_KEY).unwrap();
        let spawn = Pos::new(0.0, 0.0, 0.0);
        let mut bot = Bot::new(BotKind::Circle, spawn, area.bounds(), f32::NAN);
        assert!(!bot.step(area.as_ref(), spawn, 0.05).moved);
        let mut bot = Bot::new(BotKind::Circle, spawn, area.bounds(), 600.0);
        assert!(!bot.step(area.as_ref(), spawn, f32::NAN).moved);
        assert!(!bot.step(area.as_ref(), spawn, -1.0).moved);
        let zero = Bounds {
            min: Pos::default(),
            max: Pos::default(),
        };
        let mut bot = Bot::new(BotKind::Line, spawn, zero, 600.0);
        let _ = bot.step(area.as_ref(), spawn, 0.05);
        // A spawn off the mesh cannot be walked on: the bot gives up instead of looping.
        let mut bot = Bot::new(BotKind::Circle, Pos::new(5000.0, 5000.0, 0.0), area.bounds(), 600.0);
        for _ in 0..200 {
            bot.step(area.as_ref(), Pos::new(5000.0, 5000.0, 0.0), 0.05);
        }
        assert!(bot.is_stuck());
    }

    /// `Corrected(p)` means "place the entity at p": a bot that finds itself off the walkable
    /// surface is put where the adapter says, instead of staying where it is.
    #[test]
    fn a_bot_off_the_mesh_is_placed_where_the_adapter_says() {
        let adapter = FixtureAdapter::new();
        let area = adapter.load_area(FIXTURE_AREA_KEY).unwrap();
        let spawn = Pos::new(-100.0, 0.0, 0.0);
        let mut bot = Bot::new(BotKind::Circle, spawn, area.bounds(), area.max_speed());
        let off = Pos::new(5000.0, 5000.0, 0.0);
        let placed = bot.step(area.as_ref(), off, 0.05);
        assert_eq!(placed.pos, area.spawn_points()[0]);
        assert!(!placed.moved && placed.facing.is_none(), "being placed is not walking");
        // From there it walks normally again.
        let next = bot.step(area.as_ref(), placed.pos, 0.05);
        assert!(next.moved && next.facing.is_some());
        assert!(placed.pos.dist_xy(next.pos) <= area.max_speed() * BOT_SPEED_FACTOR * 0.05 * 1.001);
        // A wall (Corrected onto the spot the bot already stands on) still just shrinks the
        // pattern without moving the bot.
        let corner = Pos::new(-399.9, 299.9, 0.0);
        let mut bot = Bot::new(BotKind::Circle, corner, area.bounds(), area.max_speed());
        let before = bot.size;
        let s = bot.step(area.as_ref(), corner, 0.05);
        assert!(!s.moved && s.pos == corner, "the first step from this corner leaves the mesh");
        assert!(bot.size < before, "a refused step shrinks the pattern");
    }

    // ------------------------------------------------------------------------------------
    // Party behaviour
    // ------------------------------------------------------------------------------------

    mod party {
        use super::*;
        use fmv_core::AccountId;
        use fmv_protocol::{PartyEvent, PartyFailure, PartyId, PartyName};

        const ME: AccountId = AccountId(1);
        const PEER: AccountId = AccountId(2);

        fn names() -> Vec<PartyName> {
            vec![
                PartyName { account: ME, name: "Me".into(), entity: Some(EntityId(1)) },
                PartyName { account: PEER, name: "Peer".into(), entity: Some(EntityId(2)) },
            ]
        }

        /// A mirror plus the numbering of the events the server would send it.
        struct Server {
            mirror: PartyMirror,
            seq: u64,
        }

        impl Server {
            fn new() -> Self {
                Server { mirror: PartyMirror::new(ME), seq: 0 }
            }
            fn event(&mut self, event: PartyEvent) {
                self.seq += 1;
                self.mirror.on_event(self.seq, event, names());
            }
            fn invite_sent(&mut self, id: u64, from: AccountId, to: AccountId) {
                self.event(PartyEvent::InviteSent { invite: InviteId(id), from, to, expires_at: 99 });
            }
            fn refuse(&mut self) {
                self.mirror.on_rejected(&PartyFailure::UnknownPlayer);
            }
        }

        fn ids(v: &[u64]) -> Vec<EntityId> {
            v.iter().copied().map(EntityId).collect()
        }

        #[test]
        fn a_bot_with_no_party_flags_never_sends_anything() {
            let mut bot = PartyBot::new(false, false);
            let mut s = Server::new();
            s.invite_sent(1, PEER, ME);
            assert!(bot.step(0.0, &s.mirror, &ids(&[2, 3])).is_empty());
            assert!(bot.step(5.0, &s.mirror, &ids(&[2, 3])).is_empty());
        }

        #[test]
        fn the_first_peer_is_invited_as_soon_as_someone_is_visible_and_only_once() {
            let mut bot = PartyBot::new(true, false);
            let s = Server::new();
            assert!(bot.step(0.0, &s.mirror, &[]).is_empty(), "nobody to invite yet");
            assert!(bot.step(0.5, &s.mirror, &[]).is_empty());
            // Three players show up: the LOWEST entity id is invited, whatever the order.
            let sent = bot.step(1.0, &s.mirror, &ids(&[9, 4, 6]));
            assert_eq!(sent, vec![PartyCommand::Invite(EntityId(4))]);
            // No news from the server: nothing is sent again, however long it takes.
            for t in [1.1, 2.0, 10.0, 600.0] {
                assert!(bot.step(t, &s.mirror, &ids(&[4, 6, 9])).is_empty(), "t {t}");
            }
        }

        #[test]
        fn an_announced_invite_or_a_party_ends_the_inviting() {
            // The server announces our invite: done, even if a refusal of something else follows.
            let mut bot = PartyBot::new(true, false);
            let mut s = Server::new();
            assert_eq!(bot.step(0.0, &s.mirror, &ids(&[2])).len(), 1);
            s.invite_sent(1, ME, PEER);
            s.refuse();
            assert!(bot.step(5.0, &s.mirror, &ids(&[2])).is_empty());
            assert!(bot.step(50.0, &s.mirror, &ids(&[2])).is_empty());

            // A party formed (we were invited and accepted elsewhere): done before any send.
            let mut bot = PartyBot::new(true, false);
            let mut s = Server::new();
            s.event(PartyEvent::PartyFormed { party: PartyId(1), invite: InviteId(1), leader: PEER, members: vec![PEER, ME] });
            assert!(bot.step(0.0, &s.mirror, &ids(&[2])).is_empty());
            // ...and it stays done after the party is gone again.
            s.event(PartyEvent::Disbanded { party: PartyId(1), members: vec![ME], reason: fmv_protocol::DisbandReason::LastMemberLeft });
            assert!(bot.step(9.0, &s.mirror, &ids(&[2])).is_empty());
        }

        #[test]
        fn a_refused_invite_is_retried_no_more_often_than_once_a_second_and_at_most_five_times() {
            let mut bot = PartyBot::new(true, false);
            let mut s = Server::new();
            let mut sent = Vec::new();
            let mut t = 0.0;
            assert_eq!(bot.step(t, &s.mirror, &ids(&[3, 2])), vec![PartyCommand::Invite(EntityId(2))]);
            sent.push(t);
            s.refuse();
            // Refused right away: too soon to try again.
            for dt in [0.0, 0.3, 0.9] {
                assert!(bot.step(t + dt, &s.mirror, &ids(&[2, 3])).is_empty(), "dt {dt}");
            }
            // Keep refusing every attempt, polling every 100 ms for a minute.
            let visible = ids(&[2, 3]);
            for step in 0..600 {
                t = 0.1 * f64::from(step);
                let cmds = bot.step(t, &s.mirror, &visible);
                if !cmds.is_empty() {
                    assert_eq!(cmds.len(), 1);
                    sent.push(t);
                    s.refuse();
                }
            }
            assert_eq!(sent.len(), 1 + MAX_INVITE_RETRIES as usize, "the first send plus 5 retries: {sent:?}");
            for w in sent.windows(2) {
                assert!(w[1] - w[0] >= INVITE_RETRY_S - 1e-9, "retried too soon: {sent:?}");
            }
            // And never again, whatever happens afterwards.
            s.refuse();
            assert!(bot.step(1000.0, &s.mirror, &visible).is_empty());
        }

        #[test]
        fn a_retry_goes_to_the_lowest_peer_visible_at_that_moment() {
            let mut bot = PartyBot::new(true, false);
            let mut s = Server::new();
            assert_eq!(bot.step(0.0, &s.mirror, &ids(&[2, 3])), vec![PartyCommand::Invite(EntityId(2))]);
            s.refuse();
            assert_eq!(bot.step(1.0, &s.mirror, &ids(&[3, 5])), vec![PartyCommand::Invite(EntityId(3))]);
            // Nobody visible: it waits instead of inventing a target.
            s.refuse();
            assert!(bot.step(5.0, &s.mirror, &[]).is_empty());
            assert_eq!(bot.step(6.0, &s.mirror, &ids(&[7])), vec![PartyCommand::Invite(EntityId(7))]);
        }

        #[test]
        fn a_refusal_from_before_the_first_invite_is_not_news() {
            let mut s = Server::new();
            s.refuse(); // something else of ours was refused earlier
            let mut bot = PartyBot::new(true, false);
            assert_eq!(bot.step(0.0, &s.mirror, &ids(&[2])).len(), 1);
            assert!(bot.step(3.0, &s.mirror, &ids(&[2])).is_empty(), "the old refusal does not trigger a retry");
            s.refuse();
            assert_eq!(bot.step(4.0, &s.mirror, &ids(&[2])).len(), 1, "a new refusal does");
        }

        #[test]
        fn every_invite_to_me_is_accepted_exactly_once() {
            let mut bot = PartyBot::new(false, true);
            let mut s = Server::new();
            assert!(bot.step(0.0, &s.mirror, &[]).is_empty());
            s.invite_sent(5, PEER, ME);
            s.invite_sent(6, ME, PEER); // mine: nothing to accept
            assert_eq!(bot.step(0.1, &s.mirror, &[]), vec![PartyCommand::Accept(InviteId(5))]);
            for t in [0.2, 0.3, 5.0] {
                assert!(bot.step(t, &s.mirror, &[]).is_empty(), "invite 5 was answered once: t {t}");
            }
            // A second invite is a new invite id: accepted too.
            s.invite_sent(7, AccountId(3), ME);
            assert_eq!(bot.step(6.0, &s.mirror, &[]), vec![PartyCommand::Accept(InviteId(7))]);
            // Two at once: both, in invite order, each once.
            s.invite_sent(8, AccountId(3), ME);
            s.invite_sent(9, AccountId(4), ME);
            assert_eq!(
                bot.step(7.0, &s.mirror, &[]),
                vec![PartyCommand::Accept(InviteId(8)), PartyCommand::Accept(InviteId(9))]
            );
            assert!(bot.step(8.0, &s.mirror, &[]).is_empty());
        }

        #[test]
        fn inviting_and_accepting_work_together() {
            let mut bot = PartyBot::new(true, true);
            let mut s = Server::new();
            s.invite_sent(1, PEER, ME);
            let cmds = bot.step(0.0, &s.mirror, &ids(&[2]));
            assert_eq!(cmds, vec![PartyCommand::Invite(EntityId(2)), PartyCommand::Accept(InviteId(1))]);
            assert!(bot.step(0.1, &s.mirror, &ids(&[2])).is_empty());
        }
    }
}

#[cfg(test)]
mod travel_tests {
    use super::*;
    use fmv_adapter_api::GameAdapter;
    use fmv_fixture::rooms::{RoomsAdapter, HALL, HALL_ARRIVAL, YARD, YARD_ARRIVAL};

    const DT: f32 = 0.05;

    fn rooms() -> (Box<dyn AreaRuntime>, Box<dyn AreaRuntime>) {
        let a = RoomsAdapter::new();
        (a.load_area(HALL).unwrap(), a.load_area(YARD).unwrap())
    }

    /// Walk `bot` through `area` until a step crosses an exit (returns where it ended up, the
    /// number of steps and the exit's target) or `max_steps` pass.
    fn walk_until_crossing(bot: &mut TravelBot, area: &dyn AreaRuntime, mut cur: Pos, max_steps: usize) -> Option<(Pos, usize, String)> {
        let exits = area.exits();
        let speed = area.max_speed() * BOT_SPEED_FACTOR;
        for i in 0..max_steps {
            let step = bot.step(area, cur, DT, speed);
            assert!(cur.dist_xy(step.pos) <= speed * DT * 1.001, "no step is longer than speed * dt");
            if let Some(e) = exits.iter().find(|e| e.is_crossed_by(cur, step.pos)) {
                return Some((step.pos, i + 1, e.target.clone()));
            }
            cur = step.pos;
        }
        None
    }

    #[test]
    fn a_travel_bot_walks_from_either_spawn_across_the_exit_to_the_next_area_and_then_back() {
        let (hall, yard) = rooms();
        for spawn in hall.spawn_points() {
            let mut bot = TravelBot::new(vec![YARD.into(), HALL.into()], false);
            assert!(bot.is_active());
            assert_eq!(bot.next_area(), Some(YARD));
            let (_, steps, target) = walk_until_crossing(&mut bot, hall.as_ref(), spawn, 400).expect("it reaches the exit");
            assert_eq!(target, YARD);
            assert!(steps > 3, "it walked ({steps} steps)");
            // The session says it arrived: the first stop is crossed off and the next is the way back.
            assert!(bot.arrived(YARD, 10.0));
            assert_eq!(bot.next_area(), Some(HALL));
            let arrival = Pos::new(YARD_ARRIVAL.0, YARD_ARRIVAL.1, 0.0);
            let (_, _, target) = walk_until_crossing(&mut bot, yard.as_ref(), arrival, 400).expect("it finds the way back");
            assert_eq!(target, HALL);
            assert!(bot.arrived(HALL, 20.0));
            assert!(!bot.is_active(), "the route is done");
            assert_eq!(bot.next_area(), None);
            assert_eq!(bot.gave_up(), None);
            // A finished bot does not move.
            let step = bot.step(hall.as_ref(), Pos::new(HALL_ARRIVAL.0, HALL_ARRIVAL.1, 0.0), DT, 100.0);
            assert!(!step.moved);
        }
    }

    #[test]
    fn a_travel_bot_never_crosses_an_exit_it_does_not_want() {
        // With the ghost exit at the top of the hall and the yard's behind the right edge, a bot
        // that wants the yard must not take the other one on its way.
        let adapter = RoomsAdapter::with_unloadable_area();
        let hall = adapter.load_area(HALL).unwrap();
        let exits = hall.exits();
        let mut bot = TravelBot::new(vec![YARD.into()], false);
        let speed = hall.max_speed() * BOT_SPEED_FACTOR;
        // Start right under the ghost exit (y = 250): the straight way to the yard's line is away from it.
        let mut cur = Pos::new(0.0, 240.0, 0.0);
        for _ in 0..600 {
            let step = bot.step(hall.as_ref(), cur, DT, speed);
            for e in &exits {
                if e.target != YARD {
                    assert!(!e.is_crossed_by(cur, step.pos), "crossed the exit to {}", e.target);
                }
            }
            if exits.iter().any(|e| e.target == YARD && e.is_crossed_by(cur, step.pos)) {
                return;
            }
            cur = step.pos;
        }
        panic!("never reached the yard's exit");
    }

    #[test]
    fn a_travel_bot_without_a_way_to_its_target_says_so_and_stops() {
        let (_, yard) = rooms();
        // The yard only leads back to the hall.
        let mut bot = TravelBot::new(vec!["vault".into()], false);
        let step = bot.step(yard.as_ref(), Pos::new(YARD_ARRIVAL.0, YARD_ARRIVAL.1, 0.0), DT, 100.0);
        assert!(!step.moved);
        assert!(!bot.is_active());
        let why = bot.gave_up().expect("it says why");
        assert!(why.contains("vault") && why.contains("yard"), "{why}");
    }

    #[test]
    fn arriving_somewhere_else_leaves_the_route_alone_and_names_match_in_any_case() {
        let mut bot = TravelBot::new(vec!["MD1_1".into(), "md1stin".into()], false);
        assert!(!bot.arrived("elsewhere", 1.0));
        assert_eq!(bot.next_area(), Some("MD1_1"));
        assert!(bot.arrived("md1_1", 2.0), "area keys compare without regard to case");
        assert_eq!(bot.next_area(), Some("md1stin"));
    }

    #[test]
    fn the_gate_holds_the_bot_for_its_party_and_lingers_after_an_arrival() {
        let gate = |now: f64, party_size: usize, peers_seen: usize| TravelGate { now, party_size, peers_seen };
        // Waiting for a party: not before it has two members, but never longer than PARTY_WAIT_S.
        let bot = TravelBot::new(vec![YARD.into()], true);
        assert!(!bot.may_walk(gate(0.0, 0, 0)));
        assert!(!bot.may_walk(gate(5.0, 1, 3)));
        assert!(bot.may_walk(gate(5.0, 2, 0)));
        assert!(!bot.may_walk(gate(PARTY_WAIT_S - 0.1, 1, 0)));
        assert!(bot.may_walk(gate(PARTY_WAIT_S + 0.1, 1, 0)), "it gives up waiting");
        // Without a party to form it walks at once.
        assert!(TravelBot::new(vec![YARD.into()], false).may_walk(gate(0.0, 0, 0)));
        // Nothing to do: never.
        assert!(!TravelBot::new(vec![], false).may_walk(gate(100.0, 2, 2)));

        // After arriving with more route left: at least ARRIVAL_LINGER_S, and until another player
        // was seen, but at most PEER_WAIT_S.
        let mut bot = TravelBot::new(vec![YARD.into(), HALL.into()], false);
        assert!(bot.arrived(YARD, 100.0));
        assert!(!bot.may_walk(gate(100.5, 0, 5)), "too soon, whoever is there");
        assert!(!bot.may_walk(gate(103.0, 0, 0)), "nobody seen yet");
        assert!(bot.may_walk(gate(103.0, 0, 1)), "someone was seen");
        assert!(!bot.may_walk(gate(100.0 + PEER_WAIT_S - 0.1, 0, 0)));
        assert!(bot.may_walk(gate(100.0 + PEER_WAIT_S + 0.1, 0, 0)), "it gives up waiting for company");
    }

    #[test]
    fn a_pattern_bot_never_walks_across_an_exit() {
        use crate::cli::BotKind;
        let (hall, _) = rooms();
        // A circle centred a few steps from the hall's exit line: its radius would carry it over.
        let spawn = Pos::new(EXIT_X_NEAR, 0.0, 0.0);
        let exits = hall.exits();
        for kind in [BotKind::Circle, BotKind::Line] {
            let mut bot = Bot::new(kind, spawn, hall.bounds(), hall.max_speed());
            let mut cur = spawn;
            let mut walked = 0.0;
            for _ in 0..4000 {
                let step = bot.step(hall.as_ref(), cur, DT);
                assert!(!exits.iter().any(|e| e.is_crossed_by(cur, step.pos)), "{kind:?} crossed an exit from {cur:?}");
                walked += cur.dist_xy(step.pos);
                cur = step.pos;
            }
            assert!(walked > 100.0, "{kind:?} still walks on its own side ({walked} units)");
        }
    }

    /// Near the hall's exit line (x = 300).
    const EXIT_X_NEAR: f32 = 280.0;

    /// The L-shaped FIXTURE plaza with an exit at the far end of its arm: a notch between the main
    /// part and the arm cuts the straight way from the top left to the exit.
    struct PlazaWithExit {
        inner: Box<dyn AreaRuntime>,
        exit: Exit,
    }

    impl AreaRuntime for PlazaWithExit {
        fn info(&self) -> &fmv_adapter_api::AreaInfo {
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
        fn exits(&self) -> Vec<Exit> {
            vec![self.exit.clone()]
        }
    }

    fn plaza_with_exit() -> PlazaWithExit {
        let inner = fmv_fixture::FixtureAdapter::new().load_area(fmv_fixture::FIXTURE_AREA_KEY).unwrap();
        let exit = Exit {
            line: (Pos::new(680.0, -100.0, 0.0), Pos::new(680.0, -20.0, 0.0)),
            target: "far".into(),
            arrival: Pos::default(),
            arrival_facing: None,
            arrival_hint: None,
            kind: fmv_adapter_api::ExitKind::Gateway,
        };
        PlazaWithExit { inner, exit }
    }

    /// What the server does with the chord between two announced positions: walk it in short steps
    /// (at most `max_speed / 60` each), every one accepted from where the last one ended.
    fn server_accepts_chord(area: &dyn AreaRuntime, from: Pos, to: Pos) -> bool {
        let step = f64::from(area.max_speed()) / 60.0;
        let dist = f64::from(from.dist_xy(to));
        let n = ((dist / step).ceil() as usize).clamp(1, 64);
        let mut cur = from;
        for i in 1..=n {
            let t = i as f32 / n as f32;
            let want = Pos::new(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t, cur.z);
            match area.validate_move(cur, want) {
                MoveResult::Accepted(p) => cur = p,
                _ => return false,
            }
        }
        true
    }

    #[test]
    fn a_travel_bot_plans_around_a_corner_and_its_announcements_are_chords_the_server_accepts() {
        let area = plaza_with_exit();
        let start = Pos::new(-350.0, 280.0, 0.0);
        // The straight way to the exit crosses the notch: the greedy fan alone would have to slide.
        let mid = area.exit.midpoint();
        assert!(!server_accepts_chord(&area, start, mid), "the premise: the straight way is blocked");
        let mut bot = TravelBot::new(vec!["far".into()], false);
        let speed = area.max_speed() * BOT_SPEED_FACTOR;
        let mut cur = start;
        let mut positions = vec![cur];
        let mut crossed_at = None;
        for i in 0..4000 {
            let step = bot.step(&area, cur, DT, speed);
            assert!(cur.dist_xy(step.pos) <= speed * DT * 1.001);
            if area.exit.is_crossed_by(cur, step.pos) {
                crossed_at = Some(i);
                break;
            }
            cur = step.pos;
            positions.push(cur);
        }
        let steps = crossed_at.expect("the bot reached the exit");
        assert!(bot.gave_up().is_none(), "{:?}", bot.gave_up());
        // About the length of the detour at the bot's speed, not a wander.
        assert!((steps as f32) * speed * DT < start.dist_xy(mid) * 1.4, "{steps} steps");
        // The client announces a position every 50 ms (here: every step is 50 ms): any two announced
        // positions a few steps apart must be a chord the server walks.
        for window in positions.windows(4) {
            assert!(server_accepts_chord(&area, window[0], window[3]), "the chord {:?} -> {:?} would be refused", window[0], window[3]);
        }
    }
}
