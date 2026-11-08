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
//! animation**: the impact is never later than [`super::cue::MAX_IMPACT_DELAY`] after the batch
//! arrived, whatever the clips' lengths or what the actor was doing (a later performance of an actor
//! takes over from its start). An actor without real clips ([`Kit::none`]: a stand-in, a box) gets
//! the procedural motion that stood here first: the lunge with its hop, the blink and shake, the
//! collapse.
//!
//! The matches on actions and events here and in [`cue`] are exhaustive on purpose: protocol v7's
//! `ActionRef::Limit` (a big attack or special clip with a strong effect), `ActionRef::Escape` (the party
//! runs off the screen), `BattleEvent::EscapeFailed` (a short stumble back) and `Outcome::Escaped` (no
//! victory pose) make the compiler list every arm to add. See the TODO(v7) notes of [`cue`].

use super::clips::{Kit, Slot};
use super::cue::{self, Cue, Landing, DEATH_BEAT};
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
/// How long a beaten enemy with real clips takes to fade out, in seconds.
pub const FADE: f32 = 0.6;
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
    /// Not drawn at all any more (a collapsed or faded enemy), shadow included.
    pub gone: bool,
}

impl Default for Motion {
    fn default() -> Self {
        Self { offset: [0.0; 3], clip: None, squash: 1.0, fall: 0.0, alpha: 1.0, visible: true, gone: false }
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
}

impl Effect {
    fn actor(&self) -> usize {
        match *self {
            Effect::Lunge { actor, .. } | Effect::Flinch { actor, .. } | Effect::Dodge { actor, .. } => actor,
        }
    }

    fn start(&self) -> f64 {
        match *self {
            Effect::Lunge { lunge, .. } => lunge.start,
            Effect::Flinch { start, .. } | Effect::Dodge { start, .. } => start,
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

    /// A batch of events that arrived at `arrival`: every performance in it is scheduled, each so that
    /// its blow lands [`cue::impact_delay`] later. `look_of` says what an action looks like (from
    /// the engine: does it run up to its target, which element is it).
    pub fn on_batch(&mut self, arrival: f64, events: &[BattleEvent], look_of: &dyn Fn(ActorId, ActionRef) -> Look) {
        for cue in cue::cues(events) {
            self.perform(arrival, &cue, look_of);
        }
        if events.iter().any(|e| matches!(e, BattleEvent::Victory)) {
            self.on_victory(arrival);
        }
    }

    /// One performance: the performer's clips and run, what lands on whom, the effects.
    fn perform(&mut self, arrival: f64, cue: &Cue<'_>, look_of: &dyn Fn(ActorId, ActionRef) -> Look) {
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
            // TODO(v7): `Some(ActionRef::Limit(_))` plays the performer's special clip with a strong
            // effect; `Some(ActionRef::Escape)` is `Shape::Guard` with no hold and starts the run off.
            let shape = match action {
                None | Some(ActionRef::Attack) => self.strike_shape(kit, None, melee, reach, pi),
                Some(ActionRef::EnemyAttack(id)) => self.strike_shape(kit, Some(id), melee, reach, pi),
                Some(ActionRef::Magic(_)) if self.sides[pi] == Side::Party => Shape::Cast,
                Some(ActionRef::Magic(_)) => Shape::Stand { strike: kit.plain.map(|slot| plan::Strike { slot, impact: 0.5 }) },
                Some(ActionRef::Item(_)) => Shape::Stand { strike: plan::item_strike(kit) },
                Some(ActionRef::Defend) => Shape::Guard,
            };
            let performance = plan::plan(kit, shape, arrival, cue.impact());
            let cap = |v: &mut Vec<Segment>| {
                if v.len() > MAX_SEGMENTS {
                    v.drain(..v.len() - MAX_SEGMENTS);
                }
            };
            self.segments[pi].extend(performance.segments);
            cap(&mut self.segments[pi]);
            if let Some(lunge) = performance.lunge {
                self.effects.push(Effect::Lunge { actor: pi, lunge, reach, dir });
            }
            // A spell's caster glows while it gathers the spell.
            if matches!(action, Some(ActionRef::Magic(_))) {
                self.charges.push(Charge { caster: pi, start: arrival, end: impact, element: look.element });
            }
        }
        // What lands on whom, at the impact.
        for event in cue.events {
            match cue::landing(event) {
                Some(Landing::Impact { target }) => self.land(event, target, impact, dir, action, look),
                Some(Landing::Death { actor }) => {
                    if let Some((i, _)) = self.anchor(actor) {
                        if self.deaths[i].is_none() {
                            self.deaths[i] = Some(impact + DEATH_BEAT);
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

    /// The shape of a blow: runs up to its target when `melee` and there is ground to cover, plays the
    /// clip its attack names (for an enemy: `attack`'s slot, else its first attack's), or a party
    /// member's own attack clip.
    fn strike_shape(&self, kit: &Kit, attack: Option<u16>, melee: bool, reach: f32, actor: usize) -> Shape {
        let strike = match self.sides[actor] {
            Side::Party => plan::party_strike(kit),
            Side::Enemy => attack.and_then(|id| kit.attack_slot(id)).or(kit.plain).map(|slot| plan::Strike { slot, impact: 0.5 }),
        };
        if melee {
            Shape::Melee { reach, strike }
        } else {
            Shape::Stand { strike }
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
                self.burst(impact, i, if matches!(action, Some(ActionRef::Magic(_))) { BurstKind::Spell } else { BurstKind::Hit { critical } }, look.element);
            }
            BattleEvent::Miss { .. } => {
                // Step to alternating sides, so a run of misses does not all go the same way.
                let side = if i % 2 == 0 { 1.0 } else { -1.0 };
                self.effects.push(Effect::Dodge { actor: i, start: impact, dir: [-dir[1] * side, dir[0] * side] });
            }
            BattleEvent::Heal { .. } | BattleEvent::Revive { .. } | BattleEvent::MpRestored { .. } => {
                let kind = if matches!(action, Some(ActionRef::Magic(_))) { BurstKind::Spell } else { BurstKind::Item };
                self.burst(impact, i, kind, if look.element == Element::Plain { Element::Cure } else { look.element });
            }
            // Not a landing at the target (the cue's own `landing` does not make these).
            BattleEvent::Death { .. }
            | BattleEvent::GaugeReady { .. }
            | BattleEvent::Victory
            | BattleEvent::Defeat
            | BattleEvent::Action { .. }
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
