//! Getting about in an area whose surface is cut into pieces that only LINKS join: the ladders and jumps of the
//! first reactor (`fmv_adapter_api::Link`). A bot that only plans over the walkable surface cannot get from one piece
//! to the next, and walking up to a jump's line by accident sends it back where it came from, so a bot in an area with
//! links walks with a [`Navigator`].
//!
//! # The route
//!
//! A route is a few walks and the links between them. [`plan_route`] searches it breadth first over the places a
//! walker can be: where it stands now, and where each link puts it (`AreaRuntime::place_arrival` of the link's
//! destination, the very place the server puts the player at). From each place it asks the planner ([`crate::nav`])
//! whether the goal can be reached on foot, and failing that which links' lines can: the fewest links win. A walk-on
//! link is fired by walking up to its line; one that needs OK ([`Link::needs_ok`]) by standing at its foot and
//! pressing OK, which is `ClientMsg::Interact`: the navigator says so ([`Nav::Press`], [`Navigator::wants_press`]) and
//! the caller sends it when nothing is unsent (see [`crate::state::OwnPlayer::has_unsent`]) and tells the navigator
//! ([`Navigator::pressed`]).
//!
//! # What must not be touched
//!
//! Every walk-on link that is not the one the walker means to fire is an obstacle: the planner is told not to take a
//! step that touches its line while the link is ARMED. The navigator keeps the server's own arming rule
//! ([`LinkArming`]: a player placed anywhere has every line within reach disarmed and must leave a line before it
//! fires again) from the positions it sees, so it knows which lines it may walk over (the one it was put down next
//! to), and never plans to fire a link that is disarmed by walking straight at it: it walks out of its reach first.
//! The arming is judged from what the walker really did ([`Navigator::observe`], called every tick): a step is a step,
//! a jump is a placement (the server moved the player: a link fired, an exit was crossed, a correction came).
//!
//! # What the server does with it
//!
//! A link that fires puts the player at its destination and tells the client with `MoveRejected { seq, corrected }`,
//! the message that already puts a client where the server says it is; the client takes it only when `seq` is its
//! newest intent, and the server answers every intent from the old place with the destination until one agrees
//! (the fence). So the navigator stands still while it waits for a link to fire, and takes the jump when it
//! comes: a position within a few units of where the link leads is the link having fired ([`LANDED`]); a position
//! anywhere else is a surprise, and the route is made again from where the walker is.
//!
//! The server decides, as always: a link that was expected to fire and did not (a press that reached nothing, an arming
//! that was not what was thought) is tried again, walking out of reach and back for a walk-on one, and given up after
//! a few tries (the route is then made without it).

use crate::bot::BotStep;
use crate::nav::{plan_path_at, plan_path_avoiding, Forbidden};
use fmv_adapter_api::{AreaKey, AreaRuntime, Link, LinkArming, MoveResult};
use fmv_core::Pos;
use std::collections::BTreeSet;

/// A walker within this far (x and y) of where a link leads, having been somewhere else a moment ago, has used the link.
const LANDED: f32 = 3.0;
/// How far (in height) from where the link leads a walker may be to have used it.
const LANDED_Z: f32 = 80.0;
/// A walker that has moved this far (x, y and height) between two looks was moved by the server, not by its own step:
/// the most a bot walks in one tick is a few dozen units.
const TELEPORT: f32 = 80.0;
/// How far apart the chords are that a planned walk is cut into to see which links it fires (the server judges the
/// chords between intents, a few dozen units at a bot's pace).
const CHORD: f32 = 12.0;
/// How near the line a place has to be to stand on it for pressing (a link is touched within [`Link::TOUCH`]; a little
/// less keeps the rounding of a step from making it a miss).
const SPOT_REACH: f32 = 9.0;
/// Most planner calls one route may take (each is a search over the surface; a real field has a handful of links).
const MAX_PLANS: usize = 160;
/// Most links one route may use.
const MAX_HOPS: usize = 8;
/// Seconds a press is waited for before it is made again (a ladder answers in a round trip).
const PRESS_WAIT_S: f32 = 1.5;
/// Seconds a walk-on link is waited for after the walker came to its line before it is walked away from and back.
const FIRE_WAIT_S: f32 = 2.0;
/// Tries at one link before the route is made without it.
const MAX_TRIES: u32 = 3;
/// Routes made for one goal before the navigator gives up on it.
const MAX_REPLANS: u32 = 8;
/// Steps (ticks) to wait before planning again after a route could not be made.
const PLAN_COOLDOWN_STEPS: u32 = 100;
/// A walk is over when the walker is this near its last waypoint.
const ARRIVED: f32 = 1.0;
/// The walker is at its goal for good when it is this near it (x and y).
const AT_GOAL: f32 = 6.0;

/// The press of OK a bot wants sent (`ClientMsg::Interact`): the character it addressed, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Press {
    pub npc: Option<u32>,
}

/// A key that tells goals apart (see [`Navigator::step`]): the same parts, the same key.
pub fn goal_key(parts: &[&str]) -> u64 {
    // FNV-1a: stable, and nothing here is secret
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for part in parts {
        for byte in part.bytes().chain(std::iter::once(0xff)) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    }
    hash
}

/// One link on a route.
#[derive(Debug, Clone)]
pub struct Hop {
    /// The index of the link in the area's list.
    pub link: usize,
    /// Whether it waits for OK.
    pub press: bool,
    /// Where it puts the walker (what the server does: the area's own placement of the link's destination).
    pub to: Pos,
    /// Where the walker stands to fire it (on the line).
    pub spot: Pos,
    /// The walk from where the walker was before it (the first waypoint) to the spot (the last).
    pub walk: Vec<Pos>,
}

/// The way to a goal: the links to use, in order, each with the walk to it, and the walk from the last place to the goal.
#[derive(Debug, Clone)]
pub struct Route {
    pub hops: Vec<Hop>,
    pub last: Vec<Pos>,
}

/// The step-touches-an-armed-line test the planner is given: the walk-on links of the area that the walker does not
/// mean to fire, as obstacles, judged on the arming the walker has now.
struct Hazard<'a> {
    links: &'a [Link],
    arming: &'a LinkArming,
    /// The link the walker means to fire (not an obstacle).
    intended: Option<usize>,
}

impl Hazard<'_> {
    /// Does the step `a -> b` fire a walk-on link that is not the intended one? A step that touches a line that is
    /// disarmed is harmless as long as the walker stays within reach of it (it arms again only farther away).
    fn touches(&self, a: Pos, b: Pos) -> bool {
        self.links.iter().enumerate().any(|(i, l)| {
            !l.needs_ok
                && Some(i) != self.intended
                && l.is_touched_by(a, b)
                && (self.arming.is_armed(i) || l.distance_xy(a) > Link::REACH || l.distance_xy(b) > Link::REACH)
        })
    }
}

/// `p` is on the walkable surface where it is (not moved by the area).
fn on_surface(area: &dyn AreaRuntime, p: Pos) -> bool {
    matches!(area.validate_move(p, p), MoveResult::Accepted(q) if q.is_finite() && q.dist_xy(p) <= 0.75)
}

fn lerp(a: Pos, b: Pos, t: f32) -> Pos {
    Pos::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t, a.z + (b.z - a.z) * t)
}

/// The places on the walkable surface to stand to fire `link`, nearest the middle of the line and to `from` first (at
/// most a handful).
fn spots(area: &dyn AreaRuntime, link: &Link, from: Pos) -> Vec<Pos> {
    let (a, b) = link.line;
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len2 = dx * dx + dy * dy;
    let near = if len2 > 1e-6 { (((from.x - a.x) * dx + (from.y - a.y) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
    let mut out: Vec<Pos> = Vec::new();
    for t in [near, 0.5, 0.25, 0.75, 0.1, 0.9, 0.0, 1.0] {
        let base = lerp(a, b, t);
        // on the line, then a little off it on either side, then further round
        for (r, n) in [(0.0_f32, 1), (3.0, 8), (6.0, 8), (SPOT_REACH, 8)] {
            for k in 0..n {
                let angle = k as f32 * std::f32::consts::TAU / n as f32;
                let p = Pos::new(base.x + r * angle.cos(), base.y + r * angle.sin(), base.z);
                if link.distance_xy(p) <= SPOT_REACH && out.iter().all(|q| q.dist_xy(p) > 1.0) && on_surface(area, p) {
                    out.push(p);
                }
            }
        }
        if out.len() >= 6 {
            break;
        }
    }
    out
}

/// The links that fire, in order, along `path` for a walker that starts with `arming`: the first one is what a
/// walker that walks the path fires (and is put somewhere else by). `None`: the path fires nothing.
fn first_fired(links: &[Link], arming: &LinkArming, path: &[Pos]) -> Option<usize> {
    let mut arming = arming.clone();
    for leg in path.windows(2) {
        let (a, b) = (leg[0], leg[1]);
        let n = ((a.dist_xy(b) / CHORD).ceil() as usize).max(1);
        let mut from = a;
        for i in 1..=n {
            let to = lerp(a, b, i as f32 / n as f32);
            if let Some(fired) = arming.step(links, from, to) {
                return Some(fired);
            }
            from = to;
        }
    }
    None
}

/// The arming of a walker that has walked `path` without firing anything.
fn armed_after(links: &[Link], arming: &LinkArming, path: &[Pos]) -> LinkArming {
    let mut arming = arming.clone();
    for leg in path.windows(2) {
        let (a, b) = (leg[0], leg[1]);
        let n = ((a.dist_xy(b) / CHORD).ceil() as usize).max(1);
        let mut from = a;
        for i in 1..=n {
            let to = lerp(a, b, i as f32 / n as f32);
            let _ = arming.step(links, from, to);
            from = to;
        }
    }
    arming
}

/// A place the walker is, in the search of [`plan_route`].
struct Node {
    pos: Pos,
    arming: LinkArming,
    /// The node it was reached from, and the hop that did it.
    parent: Option<(usize, Hop)>,
}

/// What the search asks of the world.
struct Search<'a> {
    area: &'a dyn AreaRuntime,
    links: &'a [Link],
    /// Where to walk, from a place the walker may start the last walk from.
    aim: &'a dyn Fn(Pos) -> (Pos, Pos),
    /// Whether the goal's height counts.
    level: bool,
    forbidden: Forbidden<'a>,
    /// Planner calls left.
    budget: std::cell::Cell<usize>,
}

impl Search<'_> {
    fn walk(&self, from: Pos, to: Pos, level: bool, hazard: &Hazard<'_>) -> Option<Vec<Pos>> {
        let left = self.budget.get();
        if left == 0 {
            return None;
        }
        self.budget.set(left - 1);
        let forbid = |a: Pos, b: Pos| (self.forbidden)(a, b) || hazard.touches(a, b);
        if level {
            plan_path_at(self.area, from, to, &forbid)
        } else {
            plan_path_avoiding(self.area, from, to, &forbid)
        }
    }

    /// The walk from `node` to the goal, when it is on foot: to the preferred goal, or else to the fallback.
    fn to_goal(&self, node: &Node) -> Option<Vec<Pos>> {
        let hazard = Hazard { links: self.links, arming: &node.arming, intended: None };
        let (preferred, fallback) = (self.aim)(node.pos);
        let mut goals = vec![preferred];
        if fallback.dist_xy(preferred) > 0.5 {
            goals.push(fallback);
        }
        goals.into_iter().find_map(|goal| {
            let path = self.walk(node.pos, goal, self.level, &hazard)?;
            // (the planner kept clear of every armed line: nothing along the way may fire)
            first_fired(self.links, &node.arming, &path).is_none().then_some(path)
        })
    }

    /// The hop that fires link `l` from `node`, if the walker can get to its line on foot.
    fn hop(&self, node: &Node, l: usize) -> Option<Hop> {
        let link = self.links.get(l)?;
        let to = self.area.place_arrival(link.to, link.to_hint);
        if !to.is_finite() {
            return None;
        }
        let hazard = Hazard { links: self.links, arming: &node.arming, intended: Some(l) };
        // (a line none of whose first places can be walked to is on another piece of the surface: not every place of it
        // is tried)
        let mut misses = 0;
        for spot in spots(self.area, link, node.pos) {
            // The walk must fire exactly this link and nothing else, and a press must fire nothing on the way.
            let fires = |path: &[Pos]| first_fired(self.links, &node.arming, path);
            match self.walk(node.pos, spot, true, &hazard) {
                Some(path) => {
                    let good = if link.needs_ok { fires(&path).is_none() } else { fires(&path) == Some(l) };
                    if good {
                        return Some(Hop { link: l, press: link.needs_ok, to, spot, walk: path });
                    }
                }
                None => {
                    misses += 1;
                    if misses >= 2 {
                        return None;
                    }
                }
            }
            // A walk-on link that is disarmed where the walker stands (it was put down next to it) fires only after the
            // walker has been out of its reach: go round by a place out of reach.
            if !link.needs_ok && !node.arming.is_armed(l) {
                if let Some(walk) = self.detour(node, l, spot) {
                    return Some(Hop { link: l, press: false, to, spot, walk });
                }
            }
        }
        None
    }

    /// A walk from `node` to `spot` on the line of walk-on link `l` that leaves the link's reach on the way, so that it
    /// fires when the walker comes back to the line.
    fn detour(&self, node: &Node, l: usize, spot: Pos) -> Option<Vec<Pos>> {
        let link = self.links.get(l)?;
        let (a, b) = link.line;
        let (dx, dy) = (b.x - a.x, b.y - a.y);
        let len2 = dx * dx + dy * dy;
        let t = if len2 > 1e-6 { (((node.pos.x - a.x) * dx + (node.pos.y - a.y) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
        let near = lerp(a, b, t);
        for r in [Link::REACH + 10.0, Link::REACH + 25.0, Link::REACH + 45.0] {
            for k in 0..12 {
                let angle = k as f32 * std::f32::consts::TAU / 12.0;
                let e = Pos::new(near.x + r * angle.cos(), near.y + r * angle.sin(), node.pos.z);
                if !on_surface(self.area, e) || link.distance_xy(e) <= Link::REACH + 1.0 {
                    continue;
                }
                let hazard = Hazard { links: self.links, arming: &node.arming, intended: None };
                let Some(out) = self.walk(node.pos, e, true, &hazard) else { continue };
                if first_fired(self.links, &node.arming, &out).is_some() {
                    continue;
                }
                let arming = armed_after(self.links, &node.arming, &out);
                if !arming.is_armed(l) {
                    continue;
                }
                let hazard = Hazard { links: self.links, arming: &arming, intended: Some(l) };
                let Some(back) = self.walk(e, spot, true, &hazard) else { continue };
                if first_fired(self.links, &arming, &back) == Some(l) {
                    let mut walk = out;
                    walk.extend(back.into_iter().skip(1));
                    return Some(walk);
                }
            }
        }
        None
    }
}

/// The way from `from` to the goal `aim` makes of it (`aim` is given the place the last walk starts from: the goal of an
/// exit is a little past its line, on the side the walker comes from), using as few links as it takes. `arming` is the
/// walker's arming now, `bad` the links not to use, `forbidden` the steps never to take (the lines of the exits the
/// walker does not want to cross). `level`: the goal's height counts (see [`plan_path_at`]). `None` when there is no way
/// even by links.
#[allow(clippy::too_many_arguments)]
pub fn plan_route(
    area: &dyn AreaRuntime,
    links: &[Link],
    arming: &LinkArming,
    bad: &BTreeSet<usize>,
    from: Pos,
    aim: &dyn Fn(Pos) -> (Pos, Pos),
    level: bool,
    forbidden: Forbidden<'_>,
) -> Option<Route> {
    let search = Search { area, links, aim, level, forbidden, budget: std::cell::Cell::new(MAX_PLANS) };
    let mut nodes = vec![Node { pos: from, arming: arming.clone(), parent: None }];
    let mut used = vec![false; links.len()];
    let mut i = 0;
    while i < nodes.len() {
        if let Some(last) = search.to_goal(&nodes[i]) {
            // the hops, from the start
            let mut hops = Vec::new();
            let mut at = i;
            while let Some((parent, hop)) = nodes[at].parent.take() {
                hops.push(hop);
                at = parent;
            }
            hops.reverse();
            return Some(Route { hops, last });
        }
        if hops_so_far(&nodes, i) >= MAX_HOPS {
            i += 1;
            continue;
        }
        // The links whose lines can be reached from here, the nearest first.
        let here = nodes[i].pos;
        let mut order: Vec<usize> = (0..links.len()).filter(|l| !used[*l] && !bad.contains(l)).collect();
        order.sort_by(|x, y| {
            let d = |l: &usize| links.get(*l).map_or(f32::INFINITY, |link| link.distance_xy(here));
            d(x).total_cmp(&d(y))
        });
        for l in order {
            let found = search.hop(&nodes[i], l);
            if let Some(hop) = found {
                used[l] = true;
                let arriving = LinkArming::placed_at(links, hop.to);
                nodes.push(Node { pos: hop.to, arming: arriving, parent: Some((i, hop)) });
            }
        }
        i += 1;
    }
    None
}

/// How many links were used to get to node `i`.
fn hops_so_far(nodes: &[Node], i: usize) -> usize {
    let mut n = 0;
    let mut at = i;
    while let Some((parent, _)) = nodes.get(at).and_then(|node| node.parent.as_ref()) {
        n += 1;
        at = *parent;
    }
    n
}

// ---------------------------------------------------------------------------------------------
// The navigator
// ---------------------------------------------------------------------------------------------

/// What the walker is doing with its route.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Phase {
    /// Walking the current walk.
    Walk,
    /// At the foot of a link that needs OK: waiting to be allowed to press it.
    Foot,
    /// Pressed (or came to the line of a walk-on link): waiting since this time for the link to fire.
    Wait { since: f32 },
}

/// The route being walked.
#[derive(Debug, Clone)]
struct Trip {
    /// Which goal it is for (the caller's key; a new key is a new goal).
    key: u64,
    route: Route,
    /// The hop being walked to; `route.hops.len()` is the last walk.
    at: usize,
    /// The waypoint of the current walk that is next.
    next: usize,
    phase: Phase,
    /// Tries made at the current hop.
    tries: u32,
    /// The walk the current hop's walk was replaced by (a detour to arm a link again), if any.
    detour: Option<Vec<Pos>>,
    /// The walker came to the goal ([`Nav::Here`]): nothing is left of the trip but to be told so again.
    done: bool,
}

impl Trip {
    fn walk(&self) -> &[Pos] {
        match self.route.hops.get(self.at) {
            Some(hop) => self.detour.as_deref().unwrap_or(&hop.walk),
            None => &self.route.last,
        }
    }
}

/// What a [`Navigator`] says to do now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Nav {
    /// Take this step (it may be standing still: the walker waits for a link to fire).
    Step(BotStep),
    /// Press OK now, at the foot of a link that waits for it (see [`Navigator::wants_press`]); the walker stands still.
    Press,
    /// The walker is at the goal, on foot from here: the caller's own logic does the rest.
    Here,
    /// There is no way to the goal, even by links (the navigator waits a while before it tries again).
    NoWay,
}

/// Walks a bot about in an area with links (see the module docs): one per bot, called every tick.
#[derive(Debug)]
pub struct Navigator {
    area: Option<AreaKey>,
    links: Vec<Link>,
    arming: LinkArming,
    /// Where the walker was at the last look.
    seen: Option<Pos>,
    trip: Option<Trip>,
    /// Links that failed after [`MAX_TRIES`] tries in this area.
    bad: BTreeSet<usize>,
    clock: f32,
    /// Ticks to wait before planning again.
    cooldown: u32,
    /// Routes made for the goal now.
    replans: u32,
    notes: Vec<String>,
}

impl Default for Navigator {
    fn default() -> Self {
        Self::new()
    }
}

impl Navigator {
    pub fn new() -> Self {
        Self {
            area: None,
            links: Vec::new(),
            arming: LinkArming::default(),
            seen: None,
            trip: None,
            bad: BTreeSet::new(),
            clock: 0.0,
            cooldown: 0,
            replans: 0,
            notes: Vec::new(),
        }
    }

    /// The session entered another area (or the walker was put down anywhere new): what was learned of the last is of no use.
    pub fn arrived(&mut self) {
        self.area = None;
        self.links.clear();
        self.arming = LinkArming::default();
        self.seen = None;
        self.trip = None;
        self.bad.clear();
        self.cooldown = 0;
        self.replans = 0;
    }

    /// Load the links of `area` when it is not the one held; whether it has any.
    pub fn has_links(&mut self, area: &dyn AreaRuntime) -> bool {
        let key = area.info().id.key.clone();
        if self.area.as_ref() != Some(&key) {
            self.arrived();
            self.area = Some(key);
            self.links = area.links();
        }
        !self.links.is_empty()
    }

    /// Whether the walker is at the foot of an OK link and wants it pressed now.
    pub fn wants_press(&self) -> bool {
        self.trip.as_ref().is_some_and(|t| t.phase == Phase::Foot)
    }

    /// The press went out: wait for the link to fire.
    pub fn pressed(&mut self) {
        let now = self.clock;
        if let Some(trip) = self.trip.as_mut() {
            if trip.phase == Phase::Foot {
                trip.phase = Phase::Wait { since: now };
                trip.tries += 1;
                let link = trip.route.hops.get(trip.at).map(|h| h.link);
                let note = format!("pressed OK at the foot of link {}", link.map_or_else(|| "?".to_string(), |l| l.to_string()));
                self.note(note);
            }
        }
    }

    /// What the navigator wants said on stderr since the last call.
    pub fn drain_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notes)
    }

    fn note(&mut self, line: String) {
        if self.notes.len() < 64 {
            self.notes.push(line);
        }
    }

    /// Look at where the walker is: a step it made keeps the arming up to date, a jump is the server placing it (a link
    /// that fired, a correction): the arming starts over there, and the route goes on from the place the link leads to.
    /// Call it every tick, whoever moves the walker (the navigator's own `step` does too).
    pub fn observe(&mut self, cur: Pos) {
        if self.links.is_empty() || !cur.is_finite() {
            self.seen = Some(cur);
            return;
        }
        let Some(prev) = self.seen.replace(cur) else {
            self.arming = LinkArming::placed_at(&self.links, cur);
            return;
        };
        let moved = prev.dist_xy(cur).hypot(cur.z - prev.z);
        if moved < 1e-3 {
            return;
        }
        // The link it was waiting for?
        let landed = self.trip.as_ref().and_then(|t| t.route.hops.get(t.at)).is_some_and(|hop| {
            cur.dist_xy(hop.to) <= LANDED && (cur.z - hop.to.z).abs() <= LANDED_Z && moved > LANDED
        });
        if landed {
            self.arming = LinkArming::placed_at(&self.links, cur);
            let note = self.trip.as_ref().and_then(|t| t.route.hops.get(t.at)).map(|h| format!("link {} fired: now at ({:.0}, {:.0})", h.link, cur.x, cur.y));
            if let Some(trip) = self.trip.as_mut() {
                trip.at += 1;
                trip.next = 1;
                trip.phase = Phase::Walk;
                trip.tries = 0;
                trip.detour = None;
            }
            // (a link that fired is headway: the routes made for this goal so far are not held against it)
            self.replans = 0;
            if let Some(note) = note {
                self.note(note);
            }
            return;
        }
        if moved > TELEPORT {
            // moved by somebody else: placed anew, and the route made from here
            self.arming = LinkArming::placed_at(&self.links, cur);
            // (a trip that was over has nothing to be told about)
            if self.trip.take().is_some_and(|t| !t.done) {
                self.note(format!("moved to ({:.0}, {:.0}) by the server: the route is made again", cur.x, cur.y));
            }
            return;
        }
        let _ = self.arming.step(&self.links, prev, cur);
    }

    /// One tick: walk toward the goal that `aim` makes of the place a walk to it starts from, by whatever links it takes.
    /// `key` tells goals apart (a different key is a different goal and a new route); `level`: the goal's height
    /// counts; `forbidden`: steps never to take.
    #[allow(clippy::too_many_arguments)]
    pub fn step(&mut self, area: &dyn AreaRuntime, cur: Pos, key: u64, aim: &dyn Fn(Pos) -> (Pos, Pos), level: bool, forbidden: Forbidden<'_>, dt: f32, speed: f32) -> Nav {
        let still = BotStep { pos: cur, facing: None, moved: false };
        if !self.has_links(area) {
            return Nav::Here;
        }
        self.clock += dt.max(0.0);
        self.observe(cur);
        if self.trip.as_ref().is_some_and(|t| t.key != key) {
            self.trip = None;
            self.replans = 0;
            self.cooldown = 0;
        }
        if self.trip.is_none() {
            if self.cooldown > 0 {
                self.cooldown -= 1;
                return Nav::NoWay;
            }
            if self.replans >= MAX_REPLANS {
                return Nav::NoWay;
            }
            self.replans += 1;
            match plan_route(area, &self.links, &self.arming, &self.bad, cur, aim, level, forbidden) {
                Some(route) => {
                    let hops: String = route.hops.iter().map(|h| format!(" {}", h.link)).collect();
                    self.note(format!("plan: {} link(s){hops}", route.hops.len()));
                    self.trip = Some(Trip { key, route, at: 0, next: 1, phase: Phase::Walk, tries: 0, detour: None, done: false });
                }
                None => {
                    self.cooldown = PLAN_COOLDOWN_STEPS;
                    return Nav::NoWay;
                }
            }
        }
        self.follow(area, cur, speed * dt, forbidden, still)
    }

    /// The route is held: one tick of it.
    fn follow(&mut self, area: &dyn AreaRuntime, cur: Pos, len: f32, forbidden: Forbidden<'_>, still: BotStep) -> Nav {
        let Some(trip) = self.trip.as_ref() else { return Nav::NoWay };
        let (phase, hop) = (trip.phase, trip.route.hops.get(trip.at).cloned());
        match phase {
            Phase::Foot => Nav::Press,
            Phase::Wait { since } => self.wait(area, cur, since, hop, forbidden, still),
            Phase::Walk => self.walk(area, cur, len, hop, forbidden, still),
        }
    }

    /// Waiting for the link of `hop` to fire (pressed, or the walker came to its line).
    fn wait(&mut self, area: &dyn AreaRuntime, cur: Pos, since: f32, hop: Option<Hop>, forbidden: Forbidden<'_>, still: BotStep) -> Nav {
        let Some(hop) = hop else { return Nav::Step(still) };
        let limit = if hop.press { PRESS_WAIT_S } else { FIRE_WAIT_S };
        if self.clock - since <= limit {
            return Nav::Step(still);
        }
        // It did not fire.
        let tries = self.trip.as_ref().map_or(MAX_TRIES, |t| t.tries);
        if tries >= MAX_TRIES {
            self.bad.insert(hop.link);
            self.trip = None;
            self.note(format!("link {} did not fire after {MAX_TRIES} tries: the route is made without it", hop.link));
            return Nav::Step(still);
        }
        if hop.press {
            if let Some(trip) = self.trip.as_mut() {
                trip.phase = Phase::Foot;
            }
            return Nav::Press;
        }
        // A walk-on link: walk away out of its reach and back (the walker may have been put down next to it).
        let same = |p: Pos| (p, p);
        let search = Search { area, links: &self.links, aim: &same, level: false, forbidden, budget: std::cell::Cell::new(MAX_PLANS) };
        let node = Node { pos: cur, arming: LinkArming::placed_at(&self.links, cur), parent: None };
        match search.detour(&node, hop.link, hop.spot) {
            Some(walk) => {
                if let Some(trip) = self.trip.as_mut() {
                    trip.detour = Some(walk);
                    trip.next = 1;
                    trip.phase = Phase::Walk;
                }
            }
            None => {
                self.bad.insert(hop.link);
                self.trip = None;
                self.note(format!("link {} did not fire and there is no way to walk out of its reach: the route is made without it", hop.link));
            }
        }
        Nav::Step(still)
    }

    /// Walking the current walk of the route.
    fn walk(&mut self, area: &dyn AreaRuntime, cur: Pos, len: f32, hop: Option<Hop>, forbidden: Forbidden<'_>, still: BotStep) -> Nav {
        let clock = self.clock;
        let Some(trip) = self.trip.as_mut() else { return Nav::NoWay };
        let walk: Vec<Pos> = trip.walk().to_vec();
        while walk.get(trip.next).is_some_and(|wp| cur.dist_xy(*wp) < ARRIVED) {
            trip.next += 1;
        }
        let Some(wp) = walk.get(trip.next).copied() else {
            // The walk is over.
            let Some(hop) = hop else {
                // The last walk: at the goal, or pushed away from it (the route is made again).
                if walk.last().is_none_or(|end| cur.dist_xy(*end) <= AT_GOAL) {
                    self.replans = 0;
                    trip.done = true;
                    return Nav::Here;
                }
                self.trip = None;
                return Nav::Step(still);
            };
            trip.detour = None;
            if hop.press {
                // At the foot of the ladder: the line must be under the walker's feet.
                if self.links.get(hop.link).is_some_and(|l| l.is_touched_at(cur)) {
                    trip.phase = Phase::Foot;
                    return Nav::Press;
                }
                self.bad.insert(hop.link);
                self.trip = None;
                self.notes.push(format!("not at the foot of link {} (its line is out of reach): the route is made without it", hop.link));
                return Nav::Step(still);
            }
            trip.phase = Phase::Wait { since: clock };
            trip.tries += 1;
            return Nav::Step(still);
        };
        let intended = hop.as_ref().filter(|h| !h.press).map(|h| h.link);
        let dist = cur.dist_xy(wp);
        let (ux, uy) = ((wp.x - cur.x) / dist, (wp.y - cur.y) / dist);
        let step_len = len.min(dist);
        let want = Pos::new(cur.x + ux * step_len, cur.y + uy * step_len, cur.z);
        let accepted = match area.validate_move(cur, want) {
            MoveResult::Accepted(p) if p.is_finite() && !forbidden(cur, p) => Some(p),
            _ => None,
        };
        // A step that would fire a link the walker does not mean to is not taken.
        let accepted = accepted.filter(|p| {
            let mut arming = self.arming.clone();
            arming.step(&self.links, cur, *p).is_none_or(|fired| Some(fired) == intended)
        });
        match accepted {
            Some(p) => {
                if step_len >= dist {
                    trip.next += 1;
                }
                // The arming follows the step the walker is about to make.
                self.seen = Some(p);
                let _ = self.arming.step(&self.links, cur, p);
                Nav::Step(BotStep { pos: p, facing: Some(uy.atan2(ux)), moved: true })
            }
            None => {
                // The walk is blocked after all: the route is made again after a pause.
                self.trip = None;
                self.cooldown = PLAN_COOLDOWN_STEPS;
                Nav::Step(still)
            }
        }
    }

    /// The links of the area held (what `Navigator::has_links` loaded).
    #[cfg(test)]
    pub fn links(&self) -> &[Link] {
        &self.links
    }
}

#[cfg(test)]
pub(crate) mod tests;

#[cfg(test)]
mod real_tests;
