//! What the actors do: breathe, lunge at their target, flinch, dodge, fall.
//!
//! The engine's events say WHAT happened, instantly; this module spreads it over time. It is a pure
//! function of the scene clock `t` (seconds) and the effects started so far, so every phase is
//! unit-tested without a window. The [`Director`] keeps the effects and answers, for any actor and
//! any `t`, a [`Motion`]: where it is displaced to, whether it is drawn at all this instant (a hit
//! actor BLINKS, as the game's does), how flat it has collapsed or how far it has fallen over.
//!
//! # Timeline of a blow
//!
//! ```text
//! event arrives (t0)                      attacker lunges at the target ........ comes back
//!                                         |<-- out -->|<- hold ->|<-- back -->|
//! impact (t0 + out)                                   target flinches: knock-back, shake, blink
//! death, if the blow killed (impact + FLINCH_BLINK)   enemy: collapses (blinking) and is gone;
//!                                                     party member: falls over and stays down
//! ```
//!
//! A miss is the same lunge with a side-step instead of the flinch. Blows that arrive together
//! are played one after the other for the same attacker (it cannot lunge twice at once).
//!
//! # Adding an event
//!
//! [`scene_event`] reads the engine's events; kinds it does not know are ignored, so a newer
//! engine's events never break the scene. To play one, add a [`SceneEvent`] variant for it, an arm
//! in [`scene_event`] and its reaction in [`Director::on_event`]. For protocol v6: `Action` is the
//! attacker's lunge or a cast pose (today the lunge starts with the damage), `Heal` a green number
//! over the target, `Revive` the target standing up (the engine state already does this, see
//! [`Director::sync`]).

use fmv_ff7_battle::rules::{ActorId, BattleEvent, Side};

/// Space between an attacker that has run up to its target and the target, in battle units.
pub const LUNGE_GAP: f32 = 160.0;
/// How long the lunge's holding pose lasts, in seconds.
pub const LUNGE_HOLD: f32 = 0.12;
/// The run up to the target takes this long at the least and at the most, in seconds (it is the
/// delay between the announcement of a blow and its landing, which the overlay's number shows no
/// delay for: kept short).
pub const MIN_LUNGE_OUT: f32 = 0.18;
pub const MAX_LUNGE_OUT: f32 = 0.30;
/// How long a flinch blinks (a critical blow: [`FLINCH_BLINK_CRITICAL`]).
pub const FLINCH_BLINK: f32 = 0.42;
pub const FLINCH_BLINK_CRITICAL: f32 = 0.66;
/// Half a blink, in seconds: hidden for this long, then drawn for this long.
const BLINK_HALF: f32 = 0.06;
/// How long the flinch's shake and knock-back last.
const FLINCH_SHAKE: f32 = 0.30;
/// How long an enemy takes to collapse, and a party member to fall over.
pub const COLLAPSE: f32 = 0.85;
pub const FALL: f32 = 0.55;
/// A death that was not seen coming (the engine says dead, no event was played) is already over
/// by this long.
const LONG_AGO: f64 = 1000.0;
/// A blow that would have to wait longer than this behind the ones before it (a window that was not
/// drawing, a flood of messages) is not played: the engine's state, which the scene follows, is
/// right regardless, and a backlog of lunges would only make the picture lag the fight.
pub const MAX_BACKLOG: f64 = 2.5;

/// What happened, as the scene tells it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SceneEvent {
    /// `attacker` struck `target`.
    Hit { attacker: ActorId, target: ActorId, critical: bool },
    /// `attacker`'s blow missed `target`.
    Miss { attacker: ActorId, target: ActorId },
    /// `actor` was reduced to 0 HP.
    Death { actor: ActorId },
}

/// The scene's reading of one engine event; `None` for those it does not play (gauges filling and
/// the end of the battle, today, and whatever newer engines add). See the module docs.
pub fn scene_event(event: &BattleEvent) -> Option<SceneEvent> {
    match *event {
        BattleEvent::Damage { actor, target, critical, .. } => Some(SceneEvent::Hit { attacker: actor, target, critical }),
        BattleEvent::Miss { actor, target } => Some(SceneEvent::Miss { attacker: actor, target }),
        BattleEvent::Death { actor } => Some(SceneEvent::Death { actor }),
        _ => None,
    }
}

/// Where an actor stands and how big it is, for aiming reactions at it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Anchor {
    /// The ground position `(x, z)`.
    pub ground: [f32; 2],
    pub radius: f32,
}

/// One actor's displacement at one instant.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Motion {
    /// Added to the actor's place, in battle units (`y` points down).
    pub offset: [f32; 3],
    /// Height scale: 1 standing, towards 0 collapsing.
    pub squash: f32,
    /// 0 standing, 1 lying on its back.
    pub fall: f32,
    /// Drawn this instant (false in the dark half of a blink).
    pub visible: bool,
    /// Not drawn at all any more (a collapsed enemy), shadow included.
    pub gone: bool,
}

impl Default for Motion {
    fn default() -> Self {
        Self { offset: [0.0; 3], squash: 1.0, fall: 0.0, visible: true, gone: false }
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
    /// The attacker runs `reach` units along `dir` in `out` seconds, holds, and comes back.
    Lunge { actor: usize, start: f64, out: f32, reach: f32, dir: [f32; 2] },
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
            Effect::Lunge { start, .. } | Effect::Flinch { start, .. } | Effect::Dodge { start, .. } => start,
        }
    }

    fn duration(&self) -> f32 {
        match *self {
            Effect::Lunge { out, .. } => out + LUNGE_HOLD + out * 1.2,
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

/// Keeps the effects of a battle and says what everyone is doing at any instant.
#[derive(Debug, Clone)]
pub struct Director {
    sides: Vec<Side>,
    anchors: Vec<Anchor>,
    effects: Vec<Effect>,
    /// When each actor's death began (`None` while it lives).
    deaths: Vec<Option<f64>>,
    /// Until when each actor's lunge occupies it.
    busy_until: Vec<f64>,
}

impl Director {
    /// For actors with these sides and places, in the engine's order (party first).
    pub fn new(sides: Vec<Side>, anchors: Vec<Anchor>) -> Self {
        let n = sides.len().min(anchors.len());
        Self {
            sides: sides[..n].to_vec(),
            anchors: anchors[..n].to_vec(),
            effects: Vec::new(),
            deaths: vec![None; n],
            busy_until: vec![f64::NEG_INFINITY; n],
        }
    }

    fn anchor(&self, actor: ActorId) -> Option<(usize, Anchor)> {
        let i = usize::from(actor.0);
        self.anchors.get(i).map(|a| (i, *a))
    }

    /// Start playing `event`, which was announced at `t`. Events about actors the scene does not
    /// have are ignored.
    pub fn on_event(&mut self, t: f64, event: SceneEvent) {
        match event {
            SceneEvent::Hit { attacker, target, critical } => {
                let Some(impact) = self.strike(t, attacker, target) else { return };
                let Some((i, _)) = self.anchor(target) else { return };
                let dir = self.line(attacker, target).map_or([0.0, 1.0], |(d, _)| d);
                self.effects.push(Effect::Flinch { actor: i, start: impact, critical, dir });
            }
            SceneEvent::Miss { attacker, target } => {
                let Some(impact) = self.strike(t, attacker, target) else { return };
                let Some((i, _)) = self.anchor(target) else { return };
                let dir = self.line(attacker, target).map_or([0.0, 1.0], |(d, _)| d);
                // Step to alternating sides, so a run of misses does not all go the same way.
                let side = if i % 2 == 0 { 1.0 } else { -1.0 };
                self.effects.push(Effect::Dodge { actor: i, start: impact, dir: [-dir[1] * side, dir[0] * side] });
            }
            SceneEvent::Death { actor } => {
                let Some((i, _)) = self.anchor(actor) else { return };
                if self.deaths.get(i).is_some_and(Option::is_some) {
                    return;
                }
                // After the blow that did it has played out; at once when nothing was playing.
                let after_flinch = self
                    .effects
                    .iter()
                    .filter(|e| e.actor() == i && matches!(e, Effect::Flinch { .. }))
                    .map(Effect::end)
                    .fold(t, f64::max);
                if let Some(slot) = self.deaths.get_mut(i) {
                    *slot = Some(after_flinch);
                }
            }
        }
    }

    /// The unit direction and the distance on the ground from `from` to `to`.
    fn line(&self, from: ActorId, to: ActorId) -> Option<([f32; 2], f32)> {
        let (_, a) = self.anchor(from)?;
        let (_, b) = self.anchor(to)?;
        let dist = (b.ground[0] - a.ground[0]).hypot(b.ground[1] - a.ground[1]);
        Some((direction(a.ground, b.ground), dist))
    }

    /// Start `attacker`'s lunge at `target` (after its last one is over) and return when it lands.
    fn strike(&mut self, t: f64, attacker: ActorId, target: ActorId) -> Option<f64> {
        let (ai, a) = self.anchor(attacker)?;
        let (_, b) = self.anchor(target)?;
        let (dir, dist) = self.line(attacker, target)?;
        let reach = (dist - a.radius - b.radius - LUNGE_GAP).max(0.0);
        let out = (0.14 + reach / 12_000.0).clamp(MIN_LUNGE_OUT, MAX_LUNGE_OUT);
        let start = t.max(self.busy_until.get(ai).copied().unwrap_or(t));
        if start > t + MAX_BACKLOG {
            return None;
        }
        let effect = Effect::Lunge { actor: ai, start, out, reach, dir };
        if let Some(slot) = self.busy_until.get_mut(ai) {
            *slot = effect.end();
        }
        self.effects.push(effect);
        Some(start + f64::from(out))
    }

    /// Bring the director in line with the engine: `alive(i)` says whether actor `i` is alive
    /// (`None`: no engine to ask). An actor the engine says is dead that no death was played for is
    /// dead already (a scene that started late), and one the engine says is alive again stands up.
    pub fn sync(&mut self, t: f64, alive: impl Fn(usize) -> Option<bool>) {
        for i in 0..self.deaths.len() {
            match (alive(i), self.deaths[i]) {
                (Some(false), None) => self.deaths[i] = Some(t - LONG_AGO),
                (Some(true), Some(_)) => self.deaths[i] = None,
                _ => {}
            }
        }
    }

    /// Forget what is over (effects) as of `t`.
    pub fn prune(&mut self, t: f64) {
        self.effects.retain(|e| e.end() > t);
        self.busy_until.iter_mut().for_each(|b| {
            if *b <= t {
                *b = f64::NEG_INFINITY;
            }
        });
    }

    /// Is actor `i` dead as far as the scene shows (its death has begun)? (tests)
    #[cfg(test)]
    pub fn is_dying_or_dead(&self, i: usize, t: f64) -> bool {
        self.deaths.get(i).copied().flatten().is_some_and(|start| t >= start)
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
                Effect::Lunge { out, reach, dir, .. } => {
                    let progress = if u < out {
                        ease_out(u / out)
                    } else if u < out + LUNGE_HOLD {
                        1.0
                    } else {
                        1.0 - ease_in_out((u - out - LUNGE_HOLD) / (out * 1.2))
                    };
                    m.offset[0] += dir[0] * reach * progress;
                    m.offset[2] += dir[1] * reach * progress;
                    // A hop on the way there.
                    m.offset[1] -= 170.0 * (std::f32::consts::PI * (u / out).min(1.0)).sin();
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
        if let Some(start) = self.deaths.get(i).copied().flatten().filter(|&s| t >= s) {
            let u = (t - start) as f32;
            match self.sides.get(i) {
                Some(Side::Enemy) => {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two party members at z = +2000 and two enemies at z = -1500, all 150 wide.
    fn director() -> Director {
        let sides = vec![Side::Party, Side::Party, Side::Enemy, Side::Enemy];
        let at = |x: f32, z: f32| Anchor { ground: [x, z], radius: 150.0 };
        Director::new(sides, vec![at(-500.0, 2000.0), at(500.0, 2000.0), at(-500.0, -1500.0), at(500.0, -1500.0)])
    }

    fn hit(attacker: u8, target: u8, critical: bool) -> SceneEvent {
        SceneEvent::Hit { attacker: ActorId(attacker), target: ActorId(target), critical }
    }

    fn near(a: f32, b: f32, tol: f32) -> bool {
        (a - b).abs() <= tol
    }

    #[test]
    fn engine_events_map_to_scene_events_and_the_rest_is_ignored() {
        use fmv_ff7_battle::rules::BattleEvent as E;
        let (a, b) = (ActorId(0), ActorId(2));
        assert_eq!(scene_event(&E::Damage { actor: a, target: b, amount: 42, critical: true }), Some(hit(0, 2, true)));
        assert_eq!(scene_event(&E::Miss { actor: a, target: b }), Some(SceneEvent::Miss { attacker: a, target: b }));
        assert_eq!(scene_event(&E::Death { actor: b }), Some(SceneEvent::Death { actor: b }));
        for ignored in [E::GaugeReady { actor: a }, E::Victory, E::Defeat] {
            assert_eq!(scene_event(&ignored), None, "{ignored:?}");
        }
    }

    #[test]
    fn a_blow_makes_the_attacker_run_up_to_its_target_and_back() {
        let mut d = director();
        d.on_event(10.0, hit(0, 3, false));
        // Nothing moves before the event.
        assert_eq!(d.motion(0, 9.9), Motion::default());
        let at_start = d.motion(0, 10.0);
        assert!(at_start.offset[0].abs() < 1.0 && at_start.offset[2].abs() < 1.0);
        // Distance 3500 less both radii and the gap: the attacker ends up that much closer.
        let dist = (1000.0f32).hypot(3500.0);
        let reach = dist - 300.0 - LUNGE_GAP;
        let Effect::Lunge { out, .. } = d.effects[0] else { panic!("the first effect is the lunge") };
        let mid_hold = d.motion(0, 10.0 + f64::from(out + LUNGE_HOLD * 0.5));
        let moved = mid_hold.offset[0].hypot(mid_hold.offset[2]);
        assert!(near(moved, reach, 1.0), "{moved} vs {reach}");
        // It runs towards the target: -z from the party's side, and towards +x (the target is at x = 500).
        assert!(mid_hold.offset[2] < 0.0 && mid_hold.offset[0] > 0.0, "{mid_hold:?}");
        // The run is eased: it has covered more than half the way after half the time out.
        let half = d.motion(0, 10.0 + f64::from(out) * 0.5);
        assert!(half.offset[0].hypot(half.offset[2]) > reach * 0.6);
        // And it is back where it started when the lunge is over.
        let end = d.effects[0].end();
        let back = d.motion(0, end + 0.001);
        assert_eq!(back, Motion::default());
        let almost = d.motion(0, end - 0.01);
        assert!(almost.offset[0].hypot(almost.offset[2]) < reach * 0.05, "nearly home: {almost:?}");
        // It hops on the way there (up is negative y) and is on the ground when it holds.
        assert!(d.motion(0, 10.0 + f64::from(out) * 0.5).offset[1] < -100.0);
        assert!(mid_hold.offset[1].abs() < 1.0);
        // Others are not moved by it.
        assert_eq!(d.motion(1, 10.2), Motion::default());
    }

    #[test]
    fn the_target_flinches_when_the_blow_lands_blinking_and_shaking_then_settles() {
        let mut d = director();
        d.on_event(5.0, hit(0, 2, false));
        let Effect::Lunge { out, .. } = d.effects[0] else { panic!() };
        let impact = 5.0 + f64::from(out);
        // Before the impact the target is untouched.
        assert_eq!(d.motion(2, impact - 0.01), Motion::default());
        // From the impact on it blinks: hidden for a half blink, drawn for the next, and so on.
        let visible = |u: f64| d.motion(2, impact + u).visible;
        assert!(!visible(0.01) && visible(0.07) && !visible(0.13) && visible(0.19));
        // The blink ends and the target is whole again.
        assert!(visible(FLINCH_BLINK as f64 + 0.05));
        assert_eq!(d.motion(2, impact + f64::from(FLINCH_BLINK) + 0.05), Motion::default());
        // It was knocked back along the line of the blow (away from the attacker) early on.
        let knocked = d.motion(2, impact + 0.02);
        assert!(knocked.offset[0] != 0.0 || knocked.offset[2] != 0.0);
        // A critical blow blinks longer (still dark half a blink after a normal one is over) and
        // shakes harder.
        let mut c = director();
        c.on_event(5.0, hit(0, 2, true));
        let strong = c.motion(2, impact + 0.02);
        assert!(!c.motion(2, impact + 0.50).visible && d.motion(2, impact + 0.50).visible);
        let magnitude = |m: Motion| m.offset[0].hypot(m.offset[2]);
        assert!(magnitude(strong) > magnitude(knocked));
    }

    #[test]
    fn a_miss_is_a_lunge_and_a_side_step() {
        let mut d = director();
        d.on_event(0.0, SceneEvent::Miss { attacker: ActorId(1), target: ActorId(3) });
        let Effect::Lunge { out, .. } = d.effects[0] else { panic!() };
        let impact = f64::from(out);
        // The target steps aside across the line of the blow, then comes back; it is never hidden.
        let mid = d.motion(3, impact + f64::from(DODGE) * 0.5);
        assert!(near(mid.offset[0].hypot(mid.offset[2]), DODGE_DISTANCE, 1.0), "{mid:?}");
        assert!(mid.visible);
        // Across the line: the attack runs mostly along z, so the step is mostly along x.
        assert!(mid.offset[0].abs() > mid.offset[2].abs(), "{mid:?}");
        assert_eq!(d.motion(3, impact + f64::from(DODGE) + 0.01), Motion::default());
        // Different targets step to different sides.
        d.on_event(3.0, SceneEvent::Miss { attacker: ActorId(1), target: ActorId(2) });
        let a = d.effects.iter().find_map(|e| if let Effect::Dodge { actor: 3, dir, .. } = e { Some(*dir) } else { None }).unwrap();
        let b = d.effects.iter().find_map(|e| if let Effect::Dodge { actor: 2, dir, .. } = e { Some(*dir) } else { None }).unwrap();
        assert!(a[0] * b[0] + a[1] * b[1] < 0.0, "actors 2 and 3 step opposite ways: {a:?} {b:?}");
    }

    #[test]
    fn an_enemy_that_dies_blinks_collapses_and_is_gone_after_the_blow_has_played() {
        let mut d = director();
        d.on_event(1.0, hit(0, 2, false));
        d.on_event(1.0, SceneEvent::Death { actor: ActorId(2) });
        let flinch_end = d.effects.iter().filter(|e| matches!(e, Effect::Flinch { .. })).map(Effect::end).next().unwrap();
        // Standing (flinching) until the blow has played out.
        assert!(!d.is_dying_or_dead(2, flinch_end - 0.01));
        assert_eq!(d.motion(2, flinch_end - 0.01).squash, 1.0);
        assert!(d.is_dying_or_dead(2, flinch_end + 0.01));
        // Then it flattens, monotonically, and is gone at the end.
        let mut last = 1.0;
        for step in 1..=16 {
            let m = d.motion(2, flinch_end + f64::from(COLLAPSE) * step as f64 / 17.0);
            assert!(m.squash < last, "flatter at every step: {m:?}");
            assert!(!m.gone);
            last = m.squash;
        }
        assert!(last < 0.2);
        let gone = d.motion(2, flinch_end + f64::from(COLLAPSE) + 0.01);
        assert!(gone.gone && gone.squash < 0.05);
        // It blinks while collapsing: both phases occur.
        let phases: Vec<bool> = (0..20).map(|k| d.motion(2, flinch_end + 0.01 + k as f64 * 0.03).visible).collect();
        assert!(phases.contains(&true) && phases.contains(&false), "{phases:?}");
        // The survivor is untouched by any of it, and a second death event changes nothing.
        assert_eq!(d.motion(3, flinch_end + 0.4), Motion::default());
        let before = d.deaths.clone();
        d.on_event(9.0, SceneEvent::Death { actor: ActorId(2) });
        assert_eq!(d.deaths, before);
    }

    #[test]
    fn a_party_member_who_dies_falls_over_and_stays_down_until_revived() {
        let mut d = director();
        d.on_event(0.0, hit(2, 1, false));
        d.on_event(0.0, SceneEvent::Death { actor: ActorId(1) });
        let start = d.deaths[1].unwrap();
        assert_eq!(d.motion(1, start - 0.01).fall, 0.0);
        let falling = d.motion(1, start + f64::from(FALL) * 0.5);
        assert!(falling.fall > 0.2 && falling.fall < 0.8, "{falling:?}");
        assert!(falling.squash == 1.0 && !falling.gone, "a party member does not collapse away");
        // Down for good (long after every effect is pruned), still drawn.
        d.prune(start + 100.0);
        let down = d.motion(1, start + 100.0);
        assert_eq!((down.fall, down.gone, down.visible), (1.0, false, true));
        // The engine says alive again: standing.
        d.sync(start + 101.0, |i| Some(i != 0 && i != 2 && i != 3));
        assert!(d.motion(1, start + 101.0).fall == 0.0, "no longer down");
    }

    #[test]
    fn a_death_nobody_saw_is_already_over_and_no_engine_changes_nothing() {
        let mut d = director();
        // A scene that starts late: the engine already says enemy 3 is dead.
        d.sync(50.0, |i| Some(i != 3));
        let m = d.motion(3, 50.0);
        assert!(m.gone, "{m:?}");
        assert_eq!(d.motion(2, 50.0), Motion::default());
        // With nothing to ask, nothing changes either way.
        let mut e = director();
        e.sync(1.0, |_| None);
        assert!(e.deaths.iter().all(Option::is_none));
        // A death the director already plays is not restarted by the engine saying dead.
        let mut f = director();
        f.on_event(1.0, SceneEvent::Death { actor: ActorId(2) });
        let started = f.deaths[2];
        f.sync(1.1, |i| Some(i != 2));
        assert_eq!(f.deaths[2], started);
    }

    #[test]
    fn blows_that_arrive_together_are_played_one_after_the_other_by_the_same_attacker() {
        let mut d = director();
        d.on_event(4.0, hit(0, 2, false));
        d.on_event(4.0, hit(0, 3, false));
        let starts: Vec<f64> = d.effects.iter().filter_map(|e| if let Effect::Lunge { start, .. } = e { Some(*start) } else { None }).collect();
        assert_eq!(starts.len(), 2);
        let first_end = d.effects.iter().find(|e| matches!(e, Effect::Lunge { .. })).unwrap().end();
        assert!(near(starts[1] as f32, first_end as f32, 1e-4), "the second lunge starts when the first is over: {starts:?}");
        // Another attacker is not held up by it.
        d.on_event(4.0, hit(1, 2, false));
        let third = d.effects.iter().rev().find_map(|e| if let Effect::Lunge { actor: 1, start, .. } = e { Some(*start) } else { None }).unwrap();
        assert_eq!(third, 4.0);
    }

    #[test]
    fn a_flood_of_blows_is_not_played_as_a_backlog_that_lags_the_fight() {
        let mut d = director();
        for _ in 0..500 {
            d.on_event(10.0, hit(0, 2, false));
        }
        let lunges: Vec<f64> = d.effects.iter().filter_map(|e| if let Effect::Lunge { start, .. } = e { Some(*start) } else { None }).collect();
        assert!(!lunges.is_empty() && lunges.len() < 10, "{} lunges", lunges.len());
        assert!(lunges.iter().all(|&s| s <= 10.0 + MAX_BACKLOG), "none starts further ahead than the backlog allows");
        assert!(d.effects.len() < 30, "the rest were dropped, not queued: {}", d.pending());
        // A death still counts, whatever was dropped.
        d.on_event(10.0, SceneEvent::Death { actor: ActorId(2) });
        assert!(d.deaths[2].is_some());
    }

    #[test]
    fn nonsense_never_breaks_anything() {
        let mut d = director();
        // Actors the scene does not have, an attacker that is its own target, a target on top of it.
        d.on_event(0.0, hit(99, 2, false));
        d.on_event(0.0, hit(0, 99, false));
        d.on_event(0.0, SceneEvent::Miss { attacker: ActorId(200), target: ActorId(0) });
        d.on_event(0.0, SceneEvent::Death { actor: ActorId(77) });
        d.on_event(1.0, hit(2, 2, false));
        assert_eq!(d.effects.iter().filter(|e| matches!(e, Effect::Lunge { .. })).count(), 1, "only the self-blow was playable");
        for t in [-5.0, 0.0, 1.0, 1.3, 2.0, 1e9] {
            for i in 0..6 {
                let m = d.motion(i, t);
                assert!(m.offset.iter().all(|c| c.is_finite()) && m.squash.is_finite() && m.fall.is_finite(), "{i} at {t}: {m:?}");
            }
        }
        // No actors, no panics.
        let mut none = Director::new(vec![], vec![]);
        none.on_event(0.0, hit(0, 1, false));
        none.sync(0.0, |_| Some(false));
        none.prune(1.0);
        assert_eq!(none.motion(0, 0.0), Motion::default());
        // Mismatched lists are cut to the shorter.
        let uneven = Director::new(vec![Side::Party; 3], vec![Anchor { ground: [0.0; 2], radius: 1.0 }; 2]);
        assert_eq!(uneven.deaths.len(), 2);
    }

    #[test]
    fn idle_sway_is_small_distinct_per_actor_and_periodic() {
        for index in 0..4 {
            for step in 0..200 {
                let s = idle(index, step as f64 * 0.05);
                assert!((0.974..=1.026).contains(&s.breath), "{s:?}");
                assert!(s.sway.abs() <= 0.051 && (0.0..=22.1).contains(&s.bob), "{s:?}");
            }
        }
        assert_ne!(idle(0, 1.0), idle(1, 1.0), "actors do not breathe in step");
        let period = 3.4 * 2.0;
        let (a, b) = (idle(2, 0.7), idle(2, 0.7 + period));
        assert!(near(a.breath, b.breath, 1e-4) && near(a.sway, b.sway, 1e-4));
    }
}
