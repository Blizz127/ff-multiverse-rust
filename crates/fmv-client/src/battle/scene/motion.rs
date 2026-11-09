//! What the actors do: breathe, run up and strike, flinch, dodge, cast, fall, fade, rejoice.
//!
//! The engine's events say WHAT happened, instantly; this module spreads it over time. It is a pure
//! function of the scene clock `t` (seconds) and what was scheduled so far, so every phase is
//! unit-tested without a window. The [`Director`] keeps the schedule and answers, for any actor and
//! any `t`, a [`Motion`]: where it is displaced to, which real CLIP it plays at which frame (none:
//! its idle loop), whether it is drawn at all this instant (a hit actor with no hurt clip BLINKS),
//! how opaque it is (a beaten enemy fades) and, for an actor without clips, how flat it has
//! collapsed or how far it has fallen over.
//!
//! # A blow, as a performance
//!
//! ```text
//! batch arrives (a)                    a + impact_delay: the IMPACT, what the overlay's numbers are born at
//! |<-- run up --><-- swing to impact -->|<- follow-through ->|<-- way back -->|   (performer: clips + lunge)
//!                                       target: hurt clip (or blink and shake, or a side-step for a miss)
//!                                       an effect at the target (sparks, a spell's burst, a heal's glow)
//!                       death, if it was the killing blow: DEATH_BEAT later, a party member plays its death
//!                       clip and lies down; an enemy fades out
//! ```
//!
//! [`super::cue`] groups a batch of events into performances and says when each lands; [`super::plan`]
//! time-warps the performer's clips so that the impact frame lands then, and **nothing waits on an
//! animation**: the impact is never later than [`super::cue::MAX_IMPACT_DELAY`] after the performance
//! begins, whatever the clips' lengths or what the actor was doing (a later performance of an actor
//! takes over from its start). A performance begins with its batch, or, when it comes after another in
//! the same batch (an enemy's counter after the blow that provoked it), when that one is over
//! ([`super::cue::Cue::start`]): the two are never played over each other. An actor without real clips ([`Kit::none`]: a stand-in, a box) gets
//! the procedural motion that stood here first: the lunge with its hop, the blink and shake, the
//! collapse; so does an attack whose script cannot be played (it never borrows another attack's clips).
//! A beaten enemy whose death script plays a clip plays it and fades once the fall has been seen; the
//! others flinch and fade.
//!
//! The matches on actions and events here and in [`cue`] are exhaustive on purpose. Protocol v7: a limit
//! break ([`ActionRef::Limit`]) is a spell-like show (the performer gathers with the cast clips and a
//! glow, then a strong burst: a bloom and a gold star, breaks over the target); an escape that fails
//! ([`BattleEvent::EscapeFailed`]) makes the performer stumble a little way back; one that succeeds (an
//! [`ActionRef::Escape`] with no failure after it, and no victory pose: a battle that was escaped has no
//! `Victory`) makes the whole party turn and run off the screen with the run clip ([`Director::on_escape`]).

use super::clips::{Kit, Slot};
use super::cue::{self, Cue, Landing};
use super::effects::{Burst, BurstKind, Charge, Element, Look};
use super::plan::{self, Lunge, Segment, Shape};
use fmv_ff7_battle::rules::{ActionRef, ActorId, BattleEvent, Side};

/// Space between an attacker that has run up to its target and the target, in battle units.
pub const LUNGE_GAP: f32 = 160.0;
/// How long the lunge's holding pose lasts when no clip says, in seconds.
pub const LUNGE_HOLD: f32 = 0.12;
/// The run up to the target takes this long at the least and at the most, at the clips' own speed
/// (the plan fits it to the schedule, see [`super::plan::fit`]).
pub const MIN_LUNGE_OUT: f32 = 0.18;
pub const MAX_LUNGE_OUT: f32 = 0.30;
/// How long a flinch blinks (a critical blow: [`FLINCH_BLINK_CRITICAL`]).
pub const FLINCH_BLINK: f32 = 0.42;
pub const FLINCH_BLINK_CRITICAL: f32 = 0.66;
/// Half a blink, in seconds: hidden for this long, then drawn for this long.
const BLINK_HALF: f32 = 0.06;
/// How long the flinch's shake and knock-back last.
const FLINCH_SHAKE: f32 = 0.30;
/// How long an enemy without a death clip takes to collapse, and a party member without one to fall over.
pub const COLLAPSE: f32 = 0.85;
pub const FALL: f32 = 0.55;
/// How long a beaten enemy with real clips takes to fade out, in seconds, and how late into its death clip the fade begins
/// at the latest (an enemy that has a death clip plays it, and fades when the fall has been seen).
pub const FADE: f32 = 0.6;
pub const DEATH_FALL_MAX: f32 = 0.6;
/// A death that was not seen coming (the engine says dead, no event was played) is already over
/// by this long.
const LONG_AGO: f64 = 1000.0;
/// What the scene remembers of an actor's performances, and of the effects, at the most: a flood of
/// messages (or a window that was not drawing) cannot make the schedule grow without bound.
pub const MAX_SEGMENTS: usize = 24;
const MAX_EFFECTS: usize = 128;
const MAX_BURSTS: usize = 64;
/// How long a hurt clip plays at the most, in seconds, whatever its length.
const MAX_HURT: f64 = 0.8;
/// The party's victory poses start this far apart, in seconds, and this long after the last blow.
const VICTORY_STAGGER: f64 = 0.18;
const VICTORY_AFTER: f64 = 0.45;
/// The party members start running off this far apart, in seconds, when the party escapes.
const ESCAPE_STAGGER: f64 = 0.10;
/// How fast a runner covers the ground at the full speed it ramps up to in [`RUN_RAMP`] seconds, in battle
/// units a second, and how long it takes to be off the screen (it is gone then).
const RUN_SPEED: f32 = 3_600.0;
const RUN_RAMP: f32 = 0.35;
const RUN_OFF: f32 = 1.6;
/// A failed escape: the performer is thrown this far back and returns, in this many seconds.
const STUMBLE_DISTANCE: f32 = 280.0;
const STUMBLE: f32 = 0.55;

/// Where an actor stands and how big it is, for aiming reactions at it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Anchor {
    /// The ground position `(x, z)`.
    pub ground: [f32; 2],
    pub radius: f32,
}

/// A real clip at a frame position: what the actor's pose is made of this instant.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClipPose {
    pub slot: Slot,
    /// The frame, fractional (the pose between two frames is interpolated).
    pub frame: f32,
}

/// One actor's displacement at one instant.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Motion {
    /// Added to the actor's place, in battle units (`y` points down).
    pub offset: [f32; 3],
    /// The clip it plays now (`None`: its idle loop).
    pub clip: Option<ClipPose>,
    /// Height scale: 1 standing, towards 0 collapsing (an actor without clips).
    pub squash: f32,
    /// 0 standing, 1 lying on its back (an actor without clips).
    pub fall: f32,
    /// How opaque it is: 1 whole, towards 0 as a beaten enemy fades.
    pub alpha: f32,
    /// Drawn this instant (false in the dark half of a blink).
    pub visible: bool,
    /// Not drawn at all any more (a collapsed or faded enemy, a party that ran off), shadow included.
    pub gone: bool,
    /// A turn about the vertical axis added to the actor's facing, in radians (a party that runs away
    /// turns its back on the enemies: pi).
    pub turn: f32,
}

impl Default for Motion {
    fn default() -> Self {
        Self { offset: [0.0; 3], clip: None, squash: 1.0, fall: 0.0, alpha: 1.0, visible: true, gone: false, turn: 0.0 }
    }
}

/// The slow idle sway of a stand-in model that has no clip to play.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Idle {
    /// Height scale, close to 1.
    pub breath: f32,
    /// A small turn about the vertical axis, in radians.
    pub sway: f32,
    /// A small rise, in battle units (`y` points down, so this is subtracted).
    pub bob: f32,
}

/// The idle sway of actor `index` at `t`: a slow breath with a phase of its own, so that a row of
/// actors does not move as one.
pub fn idle(index: usize, t: f64) -> Idle {
    let phase = index as f64 * 1.9;
    let w = t * std::f64::consts::TAU / 3.4 + phase;
    Idle { breath: 1.0 + 0.025 * w.sin() as f32, sway: 0.05 * (w * 0.5).sin() as f32, bob: 22.0 * (0.5 + 0.5 * w.sin()) as f32 }
}

#[derive(Debug, Clone, Copy)]
enum Effect {
    /// The attacker runs `reach` units along `dir` (see [`Lunge`]), holds, and comes back.
    Lunge { actor: usize, lunge: Lunge, reach: f32, dir: [f32; 2] },
    /// The target is knocked back along `dir`, shakes and blinks.
    Flinch { actor: usize, start: f64, critical: bool, dir: [f32; 2] },
    /// The target steps aside, across `dir`, and back.
    Dodge { actor: usize, start: f64, dir: [f32; 2] },
    /// An actor whose escape failed is thrown back along `dir` and returns.
    Stumble { actor: usize, start: f64, dir: [f32; 2] },
}

impl Effect {
    fn actor(&self) -> usize {
        match *self {
            Effect::Lunge { actor, .. } | Effect::Flinch { actor, .. } | Effect::Dodge { actor, .. } | Effect::Stumble { actor, .. } => actor,
        }
    }

    fn start(&self) -> f64 {
        match *self {
            Effect::Lunge { lunge, .. } => lunge.start,
            Effect::Flinch { start, .. } | Effect::Dodge { start, .. } | Effect::Stumble { start, .. } => start,
        }
    }

    fn duration(&self) -> f32 {
        match *self {
            Effect::Lunge { lunge, .. } => (lunge.out + lunge.hold + lunge.back) as f32,
            Effect::Flinch { critical, .. } => {
                if critical {
                    FLINCH_BLINK_CRITICAL
                } else {
                    FLINCH_BLINK
                }
            }
            Effect::Dodge { .. } => DODGE,
            Effect::Stumble { .. } => STUMBLE,
        }
    }

    fn end(&self) -> f64 {
        self.start() + f64::from(self.duration())
    }
}

/// How long a dodge's side-step takes, out and back, and how far it goes.
const DODGE: f32 = 0.5;
const DODGE_DISTANCE: f32 = 380.0;

fn ease_out(x: f32) -> f32 {
    1.0 - (1.0 - x.clamp(0.0, 1.0)).powi(2)
}

fn ease_in(x: f32) -> f32 {
    x.clamp(0.0, 1.0).powi(2)
}

fn ease_in_out(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// The unit direction from `a` to `b` on the ground; across the field's depth when they coincide.
fn direction(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    let (dx, dz) = (b[0] - a[0], b[1] - a[1]);
    let len = dx.hypot(dz);
    if len.is_finite() && len > 1e-3 {
        [dx / len, dz / len]
    } else {
        [0.0, 1.0]
    }
}

/// Keeps the performances of a battle and says what everyone is doing at any instant.
#[derive(Debug, Clone)]
pub struct Director {
    sides: Vec<Side>,
    anchors: Vec<Anchor>,
    /// The clips of each actor ([`Kit::none`]: procedural).
    kits: Vec<Kit>,
    effects: Vec<Effect>,
    /// The clips each actor plays, as they were scheduled (the one that started last and still runs wins).
    segments: Vec<Vec<Segment>>,
    /// When each actor's death began (`None` while it lives).
    deaths: Vec<Option<f64>>,
    /// When each actor's victory pose began (`None`: not yet).
    victories: Vec<Option<f64>>,
    /// When each party member began to run off after a successful escape, and the unit direction on the
    /// ground it runs along (`None`: it stays).
    escapes: Vec<Option<(f64, [f32; 2])>>,
    /// What flashes at whom, for [`super::effects`].
    bursts: Vec<Burst>,
    charges: Vec<Charge>,
    /// When the last blow of any performance lands.
    last_impact: f64,
}

impl Director {
    /// For actors with these sides and places, in the engine's order (party first), all procedural (tests).
    #[cfg(test)]
    pub fn new(sides: Vec<Side>, anchors: Vec<Anchor>) -> Self {
        let n = sides.len().min(anchors.len());
        Self::with_kits(sides, anchors, vec![Kit::none(); n])
    }

    /// The same with the clips each actor has (an actor without an entry has none).
    pub fn with_kits(sides: Vec<Side>, anchors: Vec<Anchor>, mut kits: Vec<Kit>) -> Self {
        let n = sides.len().min(anchors.len());
        kits.resize(n, Kit::none());
        Self {
            sides: sides[..n].to_vec(),
            anchors: anchors[..n].to_vec(),
            kits,
            effects: Vec::new(),
            segments: vec![Vec::new(); n],
            deaths: vec![None; n],
            victories: vec![None; n],
            escapes: vec![None; n],
            bursts: Vec::new(),
            charges: Vec::new(),
            last_impact: f64::NEG_INFINITY,
        }
    }

    fn anchor(&self, actor: ActorId) -> Option<(usize, Anchor)> {
        let i = usize::from(actor.0);
        self.anchors.get(i).map(|a| (i, *a))
    }

    /// The unit direction and the distance on the ground from `from` to `to`.
    fn line(&self, from: ActorId, to: ActorId) -> Option<([f32; 2], f32)> {
        let (_, a) = self.anchor(from)?;
        let (_, b) = self.anchor(to)?;
        let dist = (b.ground[0] - a.ground[0]).hypot(b.ground[1] - a.ground[1]);
        Some((direction(a.ground, b.ground), dist))
    }

    /// A batch of events that arrived at `arrival`: every performance in it is scheduled, each beginning when the one
    /// before it is over ([`Cue::start`]) and so that its blow lands [`cue::impact_delay`] after that. `look_of` says what
    /// an action looks like (from the engine: does it run up to its target, which element is it).
    pub fn on_batch(&mut self, arrival: f64, events: &[BattleEvent], look_of: &dyn Fn(ActorId, ActionRef) -> Look) {
        for cue in cue::cues(events) {
            self.perform(arrival, &cue, look_of);
        }
        if events.iter().any(|e| matches!(e, BattleEvent::Victory)) {
            self.on_victory(arrival);
        }
    }

    /// One performance: the performer's clips and run, what lands on whom, the effects. It begins `cue.start` after the
    /// batch arrived at `batch`: a counter plays after the blow that provoked it, not over it.
    fn perform(&mut self, batch: f64, cue: &Cue<'_>, look_of: &dyn Fn(ActorId, ActionRef) -> Look) {
        // When THIS performance begins: everything below is scheduled from it.
        let arrival = batch + cue.start;
        let impact = arrival + cue.impact();
        let targets = cue.targets();
        let action = cue.action.map(|(_, a)| a);
        let performer = cue.performer();
        // The performer: clips and run.
        let mut dir = [0.0, 1.0];
        let mut look = Look::PLAIN;
        if let Some((pi, mine)) = performer.and_then(|p| self.anchor(p)) {
            if let (Some(p), Some(a)) = (performer, action) {
                look = look_of(p, a);
            }
            let aim = targets.iter().find_map(|&t| self.anchor(t).map(|(_, b)| (t, b)));
            let (reach, d) = match (performer, aim) {
                (Some(p), Some((t, other))) => {
                    let (d, dist) = self.line(p, t).unwrap_or(([0.0, 1.0], 0.0));
                    (((dist - mine.radius - other.radius) - LUNGE_GAP).max(0.0), d)
                }
                _ => (0.0, [0.0, 1.0]),
            };
            dir = d;
            let kit = &self.kits[pi];
            let melee = action.is_none() || look.melee;
            let shape = match action {
                None | Some(ActionRef::Attack) => self.strike_shape(kit, None, melee, reach, pi),
                Some(ActionRef::EnemyAttack(id)) => self.strike_shape(kit, Some(id), melee, reach, pi),
                // A limit break (the clips that are its own, from slot 19 up, are not decoded) is played like a
                // spell: the performer gathers its power with the cast clips and a glow, and it breaks over the
                // target with a strong effect (see `land`).
                Some(ActionRef::Magic(_) | ActionRef::Limit(_)) if self.sides[pi] == Side::Party => Shape::Cast,
                // An enemy's spell is its first attack's script, where it stands (nothing to run up to).
                Some(ActionRef::Magic(_) | ActionRef::Limit(_)) => match kit.enemy_chain(None) {
                    Some(_) => Shape::Script { reach: 0.0, attack: None },
                    None => Shape::Stand { strike: None },
                },
                // Thrown at an enemy it is the throw forward, used on an ally the gesture with the arm raised.
                Some(ActionRef::Item(_)) => {
                    let at_enemy = targets.iter().any(|&t| self.anchor(t).is_some_and(|(i, _)| self.sides[i] == Side::Enemy));
                    Shape::Stand { strike: plan::item_strike(kit, at_enemy) }
                }
                Some(ActionRef::Defend) => Shape::Guard,
                // Running has no clip of its own here: a failed attempt only stumbles (below), a successful one
                // makes the whole party run off (`on_escape`, which plays the run clip).
                Some(ActionRef::Escape) => Shape::Stand { strike: None },
            };
            let performance = plan::plan(kit, shape, arrival, cue.impact());
            let cap = |v: &mut Vec<Segment>| {
                if v.len() > MAX_SEGMENTS {
                    v.drain(..v.len() - MAX_SEGMENTS);
                }
            };
            self.segments[pi].extend(performance.segments);
            cap(&mut self.segments[pi]);
            // The plan says how far the actor goes: the whole way for a runner, a hop's length for a character whose
            // script hops.
            if let Some(lunge) = performance.lunge {
                self.effects.push(Effect::Lunge { actor: pi, lunge, reach: performance.reach, dir });
            }
            // A spell's caster glows while it gathers the spell, a limit's performer while it gathers its power.
            if matches!(action, Some(ActionRef::Magic(_) | ActionRef::Limit(_))) {
                self.charges.push(Charge { caster: pi, start: arrival, end: impact, element: look.element });
            }
            // An escape: the performer that failed is thrown back (away from the enemies); one that
            // got away takes the whole party with it.
            if matches!(action, Some(ActionRef::Escape)) {
                if cue.events.iter().any(|e| matches!(e, BattleEvent::EscapeFailed { .. })) {
                    let away = self.away_from_enemies(pi);
                    self.effects.push(Effect::Stumble { actor: pi, start: arrival, dir: away });
                } else {
                    self.on_escape(arrival);
                }
            }
        }
        // What lands on whom, at the impact.
        for event in cue.events {
            match cue::landing(event) {
                Some(Landing::Impact { target }) => self.land(event, target, impact, dir, action, look),
                Some(Landing::Death { actor }) => {
                    if let Some((i, _)) = self.anchor(actor) {
                        if self.deaths[i].is_none() {
                            // The very function the overlay's list uses: the two cannot drift apart.
                            self.deaths[i] = Some(arrival + cue::delay_of(action, event));
                        }
                    }
                }
                None => {}
            }
        }
        self.last_impact = self.last_impact.max(impact);
        self.charges.retain(|c| c.end > arrival - 1.0);
        let over = self.bursts.len().saturating_sub(MAX_BURSTS);
        self.bursts.drain(..over);
        let over = self.effects.len().saturating_sub(MAX_EFFECTS);
        self.effects.drain(..over);
    }

    /// The shape of a blow: a party member's own blow, the clips of its animation script ([`Shape::Blow`]: its short-range
    /// script when `melee`, its long-range one otherwise); an enemy's, the clips of the script `scene.bin` names for the
    /// attack ([`Shape::Script`]: the script says whether it runs up to its target, a plain attack or an attack the scene
    /// does not name plays the first attack's). An attack whose script cannot be played is the procedural motion (it
    /// does not borrow another attack's): it runs up to its target when `melee` and there is ground to cover, else stands.
    fn strike_shape(&self, kit: &Kit, attack: Option<u16>, melee: bool, reach: f32, actor: usize) -> Shape {
        match self.sides[actor] {
            Side::Party => Shape::Blow { reach, melee },
            Side::Enemy if kit.enemy_chain(attack).is_some() => Shape::Script { reach, attack },
            Side::Enemy if melee => Shape::Melee { reach, strike: None },
            Side::Enemy => Shape::Stand { strike: None },
        }
    }

    /// What `event` does to `target` when it lands at `impact`.
    fn land(&mut self, event: &BattleEvent, target: ActorId, impact: f64, dir: [f32; 2], action: Option<ActionRef>, look: Look) {
        let Some((i, _)) = self.anchor(target) else { return };
        let kit = &self.kits[i];
        match *event {
            BattleEvent::Damage { critical, .. } => {
                match kit.hurt {
                    // The clip is the flinch: it plays from the impact, at its own speed.
                    Some(slot) => {
                        let frames = kit.clips.frames_of(slot);
                        let fps = f64::from(kit.clips.fps.max(1.0));
                        let len = (f64::from(frames.saturating_sub(1)) / fps).clamp(0.15, MAX_HURT);
                        let rate = (f64::from(frames.saturating_sub(1)) / len) as f32;
                        self.segments[i].push(Segment { start: impact, len, slot, from: 0.0, rate, looping: false });
                        let over = self.segments[i].len().saturating_sub(MAX_SEGMENTS);
                        self.segments[i].drain(..over);
                    }
                    None => self.effects.push(Effect::Flinch { actor: i, start: impact, critical, dir }),
                }
                let spellish = matches!(action, Some(ActionRef::Magic(_) | ActionRef::Limit(_)));
                self.burst(impact, i, if spellish { BurstKind::Spell } else { BurstKind::Hit { critical } }, look.element);
                // A limit break breaks with a strong effect: the gold star of a critical hit on top of the bloom.
                if matches!(action, Some(ActionRef::Limit(_))) {
                    self.burst(impact, i, BurstKind::Hit { critical: true }, look.element);
                }
            }
            BattleEvent::Miss { .. } => {
                // Step to alternating sides, so a run of misses does not all go the same way.
                let side = if i % 2 == 0 { 1.0 } else { -1.0 };
                self.effects.push(Effect::Dodge { actor: i, start: impact, dir: [-dir[1] * side, dir[0] * side] });
            }
            BattleEvent::Heal { .. } | BattleEvent::Revive { .. } | BattleEvent::MpRestored { .. } => {
                let kind = if matches!(action, Some(ActionRef::Magic(_) | ActionRef::Limit(_))) { BurstKind::Spell } else { BurstKind::Item };
                self.burst(impact, i, kind, if look.element == Element::Plain { Element::Cure } else { look.element });
            }
            // A status shows as a word over the target and a tag on the panel (and, for the ones that show on the
            // body, in how it is drawn): nothing moves because of it.
            BattleEvent::StatusInflicted { .. } | BattleEvent::StatusEnded { .. } => {}
            // Not a landing at the target (the cue's own `landing` does not make these).
            BattleEvent::StatusTick { .. }
            | BattleEvent::Death { .. }
            | BattleEvent::GaugeReady { .. }
            | BattleEvent::Victory
            | BattleEvent::Defeat
            | BattleEvent::Action { .. }
            | BattleEvent::LimitGauge { .. }
            | BattleEvent::EscapeFailed { .. }
            | BattleEvent::MpSpent { .. } => {}
        }
    }

    fn burst(&mut self, at: f64, target: usize, kind: BurstKind, element: Element) {
        self.bursts.push(Burst { at, target, kind, element });
    }

    /// The battle was won at `now`: every party member still standing rejoices, one after the other,
    /// once the last blow has landed.
    pub fn on_victory(&mut self, now: f64) {
        let first = now.max(self.last_impact) + VICTORY_AFTER;
        let mut k = 0.0;
        for i in 0..self.sides.len() {
            if self.sides[i] == Side::Party && self.deaths[i].is_none() && self.victories[i].is_none() {
                self.victories[i] = Some(first + k * VICTORY_STAGGER);
                k += 1.0;
            }
        }
    }

    /// The unit direction on the ground from the living enemies' middle to actor `i`: straight away from the
    /// fight (across the field's depth when there is nobody to run from, or no enemy is left).
    fn away_from_enemies(&self, i: usize) -> [f32; 2] {
        let foes: Vec<[f32; 2]> =
            (0..self.sides.len()).filter(|&j| self.sides[j] == Side::Enemy && self.deaths[j].is_none()).map(|j| self.anchors[j].ground).collect();
        let (Some(me), false) = (self.anchors.get(i), foes.is_empty()) else { return [0.0, 1.0] };
        let n = foes.len() as f32;
        let centre = [foes.iter().map(|g| g[0]).sum::<f32>() / n, foes.iter().map(|g| g[1]).sum::<f32>() / n];
        direction(centre, me.ground)
    }

    /// The party got away at `now` (an `Escape` that no `EscapeFailed` followed): every member still standing
    /// turns its back on the enemies and runs off the screen with the run clip, one after the other, and is gone
    /// [`RUN_OFF`] seconds later. No victory pose follows: a battle that was escaped has no `Victory`.
    pub fn on_escape(&mut self, now: f64) {
        let mut k = 0.0;
        for i in 0..self.sides.len() {
            if self.sides[i] == Side::Party && self.deaths[i].is_none() && self.escapes[i].is_none() {
                let dir = self.away_from_enemies(i);
                self.escapes[i] = Some((now + k * ESCAPE_STAGGER, dir));
                k += 1.0;
            }
        }
    }

    /// Bring the director in line with the engine: `alive(i)` says whether actor `i` is alive
    /// (`None`: no engine to ask). An actor the engine says is dead that no death was played for is
    /// dead already (a scene that started late), and one the engine says is alive again stands up.
    pub fn sync(&mut self, t: f64, alive: impl Fn(usize) -> Option<bool>) {
        for i in 0..self.deaths.len() {
            match (alive(i), self.deaths[i]) {
                (Some(false), None) => self.deaths[i] = Some(t - LONG_AGO),
                (Some(true), Some(_)) => {
                    self.deaths[i] = None;
                    // Standing again: the segments that were playing when it fell are over.
                    self.segments[i].clear();
                }
                _ => {}
            }
        }
    }

    /// Forget what is over as of `t`.
    pub fn prune(&mut self, t: f64) {
        self.effects.retain(|e| e.end() > t);
        for segments in &mut self.segments {
            segments.retain(|s| s.end() > t - 0.05);
        }
        self.bursts.retain(|b| t < b.at + super::effects::BURST_LIFE);
        self.charges.retain(|c| t < c.end + 0.05);
    }

    /// The effects that flash at actors (see [`super::effects`]).
    pub fn bursts(&self) -> &[Burst] {
        &self.bursts
    }

    /// The spells that are being gathered.
    pub fn charges(&self) -> &[Charge] {
        &self.charges
    }

    /// Is actor `i` dead as far as the scene shows (its death has begun)? (tests)
    #[cfg(test)]
    pub fn is_dying_or_dead(&self, i: usize, t: f64) -> bool {
        self.deaths.get(i).copied().flatten().is_some_and(|start| t >= start)
    }

    /// The clip actor `i` plays at `t`: the segment that started last and still runs.
    fn clip_of(&self, i: usize, t: f64) -> Option<ClipPose> {
        let kit = self.kits.get(i)?;
        let seg = self.segments.get(i)?.iter().filter(|s| s.start <= t && t < s.end()).max_by(|a, b| a.start.total_cmp(&b.start))?;
        Some(ClipPose { slot: seg.slot, frame: seg.frame_at(t - seg.start, kit.clips.frames_of(seg.slot)) })
    }

    /// What actor `i` is doing at `t`.
    pub fn motion(&self, i: usize, t: f64) -> Motion {
        let mut m = Motion::default();
        for effect in self.effects.iter().filter(|e| e.actor() == i && e.start() <= t) {
            let u = (t - effect.start()) as f32;
            if u > effect.duration() {
                continue;
            }
            match *effect {
                Effect::Lunge { lunge, reach, dir, .. } => {
                    let (out, hold, back) = (lunge.out as f32, lunge.hold as f32, lunge.back as f32);
                    let progress = if u < out {
                        ease_out(u / out)
                    } else if u < out + hold {
                        1.0
                    } else {
                        1.0 - ease_in_out((u - out - hold) / back.max(1e-6))
                    };
                    m.offset[0] += dir[0] * reach * progress;
                    m.offset[2] += dir[1] * reach * progress;
                    // A hop on the way there (an actor with a run clip does not hop).
                    if out > 0.0 {
                        m.offset[1] -= lunge.hop * (std::f32::consts::PI * (u / out).min(1.0)).sin();
                    }
                }
                Effect::Flinch { critical, dir, .. } => {
                    let blink = if critical { FLINCH_BLINK_CRITICAL } else { FLINCH_BLINK };
                    if u < blink && ((u / BLINK_HALF) as u32).is_multiple_of(2) {
                        m.visible = false;
                    }
                    let x = (u / FLINCH_SHAKE).clamp(0.0, 1.0);
                    let amplitude = if critical { 110.0 } else { 60.0 };
                    let knock = amplitude * 2.0 * (1.0 - x).powi(2);
                    let shake = amplitude * (1.0 - x) * (std::f32::consts::TAU * 20.0 * u).sin();
                    m.offset[0] += dir[0] * knock - dir[1] * shake;
                    m.offset[2] += dir[1] * knock + dir[0] * shake;
                }
                Effect::Dodge { dir, .. } => {
                    let bump = (std::f32::consts::PI * (u / DODGE)).sin();
                    m.offset[0] += dir[0] * DODGE_DISTANCE * bump;
                    m.offset[2] += dir[1] * DODGE_DISTANCE * bump;
                }
                Effect::Stumble { dir, .. } => {
                    // Thrown back at once (the farthest after a third of the time), then comes back.
                    let back = (std::f32::consts::PI * ease_out(u / STUMBLE)).sin().max(0.0);
                    m.offset[0] += dir[0] * STUMBLE_DISTANCE * back;
                    m.offset[2] += dir[1] * STUMBLE_DISTANCE * back;
                }
            }
        }
        let kit = self.kits.get(i);
        m.clip = self.clip_of(i, t);
        // Victory: the pose, from its start, held on its last frame.
        if let (Some(start), Some(kit)) = (self.victories.get(i).copied().flatten().filter(|&s| t >= s), kit) {
            if let Some(slot) = kit.victory {
                let last = f32::from(kit.clips.frames_of(slot).saturating_sub(1));
                m.clip = Some(ClipPose { slot, frame: ((t - start) as f32 * kit.clips.fps).clamp(0.0, last) });
            }
        }
        // Running away (a successful escape): the run clip loops, the actor turns its back on the fight and
        // runs off, faster as it gets going; once it is off the screen it is gone.
        if let Some((start, dir)) = self.escapes.get(i).copied().flatten().filter(|&(s, _)| t >= s) {
            let u = (t - start) as f32;
            if let Some(slot) = kit.and_then(|k| k.run) {
                let frames = f32::from(kit.map_or(1, |k| k.clips.frames_of(slot)).max(1));
                m.clip = Some(ClipPose { slot, frame: (u * kit.map_or(15.0, |k| k.clips.fps)).rem_euclid(frames) });
            }
            let covered = RUN_SPEED * u * ease_in((u / RUN_RAMP).min(1.0));
            m.offset[0] += dir[0] * covered;
            m.offset[2] += dir[1] * covered;
            m.turn = std::f32::consts::PI;
            if u >= RUN_OFF {
                m.gone = true;
            }
        }
        if let Some(start) = self.deaths.get(i).copied().flatten().filter(|&s| t >= s) {
            let u = (t - start) as f32;
            match (self.sides.get(i), kit) {
                // A party member with a death clip plays it and then lies in the dead pose.
                (Some(Side::Party), Some(kit)) if kit.death.is_some() => {
                    let slot = kit.death.unwrap_or(0);
                    let last = f32::from(kit.clips.frames_of(slot).saturating_sub(1));
                    let frame = u * kit.clips.fps;
                    m.clip = Some(match kit.dead {
                        Some(dead) if frame >= last => ClipPose { slot: dead, frame: 0.0 },
                        _ => ClipPose { slot, frame: frame.min(last) },
                    });
                    m.offset = [m.offset[0], 0.0, m.offset[2]];
                }
                // An enemy whose death script plays a clip plays it, from the start of its death, and fades out once the fall
                // has been seen: half way through the clip, at the latest [`DEATH_FALL_MAX`] seconds in.
                (Some(Side::Enemy), Some(kit)) if kit.death.is_some() => {
                    let slot = kit.death.unwrap_or(0);
                    let fps = kit.clips.fps.max(1.0);
                    let last = f32::from(kit.clips.frames_of(slot).saturating_sub(1));
                    m.clip = Some(ClipPose { slot, frame: (u * fps).min(last) });
                    let fade_from = (last / fps * 0.5).min(DEATH_FALL_MAX);
                    m.alpha = 1.0 - ease_in(((u - fade_from) / FADE).max(0.0));
                    if u >= fade_from + FADE {
                        m.gone = true;
                    }
                }
                // An enemy with clips flinches (its hurt clip, played from the blow) and fades out.
                (Some(Side::Enemy), Some(kit)) if kit.hurt.is_some() => {
                    m.alpha = 1.0 - ease_in(u / FADE);
                    if u >= FADE {
                        m.gone = true;
                    }
                }
                (Some(Side::Enemy), _) => {
                    m.squash = 1.0 - 0.97 * ease_in(u / COLLAPSE);
                    // Blinking out, faster than the hit's blink, until it has gone.
                    if u < COLLAPSE * 0.7 && ((u / (BLINK_HALF * 1.2)) as u32) % 2 == 1 {
                        m.visible = false;
                    }
                    if u >= COLLAPSE {
                        m.gone = true;
                    }
                }
                _ => m.fall = ease_in_out(u / FALL),
            }
        }
        m
    }

    /// How many effects are still being played (tests).
    #[cfg(test)]
    pub fn pending(&self) -> usize {
        self.effects.len()
    }

    /// The clips scheduled for actor `i`, oldest first (tests).
    #[cfg(test)]
    pub fn segments_of(&self, i: usize) -> &[Segment] {
        self.segments.get(i).map_or(&[], Vec::as_slice)
    }

    /// When the last blow of any performance lands (tests).
    #[cfg(test)]
    pub fn last_impact(&self) -> f64 {
        self.last_impact
    }
}

#[cfg(test)]
mod tests;
