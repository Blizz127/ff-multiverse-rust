//! Choreographing one performance of the real clips so that its impact lands on schedule.
//!
//! The overlay's numbers are born at `arrival + `[`cue::impact_delay`] (see [`super::cue`]); the
//! scene has to make the blow's impact frame land at that very instant whatever the length of the
//! clips involved. This module is the whole of that arithmetic, pure and tested without a window:
//!
//! * [`fit`] fits a plan whose natural length BEFORE the impact is `pre` seconds into the time the
//!   schedule allows (`budget`): it plays faster (up to [`MAX_SPEEDUP`]) when the plan is longer, a
//!   little slower (down to [`MIN_SPEED`]) or later when it is shorter, and when even the fastest
//!   is not fast enough it **skips the start of the plan** (the run-up, the first part of a cast)
//!   instead of lagging. Whatever happens, the impact is at `arrival + budget`.
//! * [`plan`] turns a [`Shape`] (run up and strike; stand and strike; a party member's blow as its own
//!   scripts play it; cast; use an item; guard) and the actor's [`Kit`] into the [`Segment`]s of clips
//!   it plays and the [`Lunge`] it runs. After the impact the clips play at their own speed; a later
//!   performance of the same actor simply takes over from its start ([`super::motion`] plays the
//!   segment that started last), which is what keeps a fast actor from lagging behind its own commands.
//!
//! A party member's blow ([`Shape::Blow`]) is the clips of its own animation script ([`Chain`]): the
//! approach (a crouch and a leap, a dash, a hop), the strike with the blow landing on the tick the script
//! marks the damage at, the way back. Each clip plays once; everything before the impact is fitted to the
//! schedule like any other plan.
//!
//! The tail of a performance (the follow-through, the way back) is the only part that is not
//! time-warped, and it never delays anything: nothing waits for it.

use super::clips::{party, Chain, Kit, Slot};
use super::cue::MAX_IMPACT_DELAY;
use super::motion::{MAX_LUNGE_OUT, MIN_LUNGE_OUT};

/// The slowest a plan is played to fill its budget (a plan shorter than this makes the actor wait
/// before it starts instead).
pub const MIN_SPEED: f64 = 0.6;
/// The fastest the part before the impact is played. Beyond it the start of the plan is skipped.
pub const MAX_SPEEDUP: f64 = 3.0;
/// How long a pose that is only one or two frames (a held aim) is held after the impact, in seconds.
pub const HOLD_POSE: f64 = 0.40;
/// The follow-through after the impact is at least and at most this long, in seconds.
const MIN_FOLLOW: f64 = 0.20;
const MAX_FOLLOW: f64 = 0.90;
/// The way back is at least and at most this long when a clip plays it, in seconds.
const MIN_BACK: f64 = 0.25;
const MAX_BACK: f64 = 0.70;
/// How long a guard is held, a spell's pose after the impact before its end clip, in seconds.
pub const GUARD_HOLD: f64 = 1.4;
const CAST_HOLD: f64 = 0.35;
/// A clip of this many frames or fewer is a held pose, not a motion.
pub const HELD_POSE_FRAMES: u16 = 3;
/// The hop of the run-up of an actor that has no run clip (the old procedural lunge), in battle units.
pub const PROCEDURAL_HOP: f32 = 170.0;
/// The shortest the travel of a blow's approach takes, in seconds: even a plan whose approach was cut short by
/// [`fit`] closes in over this long and never jumps to its target.
pub const MIN_TRAVEL: f64 = 0.12;

/// How a plan fits its schedule (see [`fit`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Timing {
    /// When the plan starts, in seconds: the arrival, or later for a plan that is short.
    pub start: f64,
    /// How fast the part before the impact is played (1 is the clips' own speed).
    pub speed: f64,
    /// Natural seconds cut from the start of the plan (a plan too long even at [`MAX_SPEEDUP`]).
    pub skip: f64,
    /// When the impact lands: always `arrival + budget`.
    pub impact: f64,
}

/// Fit a plan that takes `pre` natural seconds to reach its impact into `budget` seconds from
/// `arrival`. The invariant, whatever the numbers: `start + (pre - skip) / speed == arrival + budget`
/// (the impact lands on schedule), `skip` is never negative and `speed` is within
/// [`MIN_SPEED`]..=[`MAX_SPEEDUP`].
pub fn fit(arrival: f64, budget: f64, pre: f64) -> Timing {
    let budget = if budget.is_finite() { budget.max(0.0) } else { MAX_IMPACT_DELAY };
    let pre = if pre.is_finite() { pre.max(0.0) } else { 0.0 };
    let impact = arrival + budget;
    if budget < 1e-6 {
        // Nothing to wait for: it all happens now.
        return Timing { start: arrival, speed: MAX_SPEEDUP, skip: pre, impact };
    }
    let wanted = pre / budget;
    if wanted < MIN_SPEED {
        // Short: play it at the slowest, starting late enough that it ends at the impact.
        Timing { start: impact - pre / MIN_SPEED, speed: MIN_SPEED, skip: 0.0, impact }
    } else if wanted <= MAX_SPEEDUP {
        Timing { start: arrival, speed: wanted, skip: 0.0, impact }
    } else {
        // Long: the fastest it can go covers `budget * MAX_SPEEDUP` of it, the beginning is dropped.
        Timing { start: arrival, speed: MAX_SPEEDUP, skip: pre - budget * MAX_SPEEDUP, impact }
    }
}

/// A run of a clip: frames `from..` of `slot` starting at `start` and lasting `len` seconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Segment {
    pub start: f64,
    pub len: f64,
    pub slot: Slot,
    /// The frame position at the start (it may be fractional) and the frames per second the clip
    /// advances at (0 holds a frame, a negative rate would run it backwards).
    pub from: f32,
    pub rate: f32,
    /// The clip runs over and over (the run), instead of stopping on its last frame.
    pub looping: bool,
}

impl Segment {
    pub fn end(&self) -> f64 {
        self.start + self.len
    }

    /// The frame position `u` seconds into the segment of a clip of `frames` frames.
    pub fn frame_at(&self, u: f64, frames: u16) -> f32 {
        let raw = self.from + self.rate * u.max(0.0) as f32;
        let n = f32::from(frames.max(1));
        if self.looping {
            raw.rem_euclid(n)
        } else {
            raw.clamp(0.0, n - 1.0)
        }
    }
}

/// The run an actor makes at its target and back.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Lunge {
    pub start: f64,
    /// Seconds out to the target, holding there, and back.
    pub out: f64,
    pub hold: f64,
    pub back: f64,
    /// The hop made on the way there (0 when a clip runs).
    pub hop: f32,
}

impl Lunge {
    /// When the run is over (tests).
    #[cfg(test)]
    pub fn end(&self) -> f64 {
        self.start + self.out + self.hold + self.back
    }
}

/// A performance as it is played.
#[derive(Debug, Clone, PartialEq)]
pub struct Performance {
    /// When the blow lands (`arrival + budget`).
    pub impact: f64,
    pub segments: Vec<Segment>,
    pub lunge: Option<Lunge>,
    /// How far along the line to its target the actor goes with [`Performance::lunge`], in battle units: the whole
    /// way for a runner, the length of a hop for a character whose script hops (0 without a lunge).
    pub reach: f32,
    /// When the last of it is over.
    pub end: f64,
}

/// The clip that lands the blow: its slot and where in it the blow lands (0 to 1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Strike {
    pub slot: Slot,
    pub impact: f32,
}

/// What the actor does.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Shape {
    /// Run up to its target (`reach` battle units away; 0: it is there), strike, come back.
    Melee { reach: f32, strike: Option<Strike> },
    /// Stay where it is and strike (a shot, an item thrown, a spell an enemy casts).
    Stand { strike: Option<Strike> },
    /// A party member's blow as its own animation scripts play it ([`Kit::blow`]: `melee` picks the script of a
    /// short-range weapon or of a long-range one): the approach, the strike with the blow landing at its marker, the
    /// way back. `reach` is how far the target is (units); a character whose script hops instead of running only
    /// covers [`party::HOP`] of it. Without a chain in the kit it is the procedural lunge of a blow with no clip.
    Blow { reach: f32, melee: bool },
    /// A party member's spell: begin, hold, end.
    Cast,
    /// Guard: hold the pose.
    Guard,
}

/// One stretch of the part before the impact, in natural time.
#[derive(Debug, Clone, Copy)]
struct Part {
    len: f64,
    clip: Option<ClipRun>,
}

#[derive(Debug, Clone, Copy)]
struct ClipRun {
    slot: Slot,
    from: f32,
    to: f32,
    looping: bool,
}

/// The parts before the impact, warped by `timing`: the segments they play and the real length of
/// each part (0 for one that was skipped). `fps` is the clips' own rate.
fn warp(parts: &[Part], timing: Timing, fps: f32) -> (Vec<Segment>, Vec<f64>) {
    let (mut segments, mut lens) = (Vec::new(), Vec::new());
    let (mut natural, mut at) = (0.0f64, timing.start);
    for part in parts {
        let begin = natural;
        natural += part.len;
        if natural <= timing.skip + 1e-12 {
            lens.push(0.0);
            continue;
        }
        let cut = (timing.skip - begin).max(0.0);
        let real = (part.len - cut) / timing.speed;
        if let Some(c) = part.clip {
            let along = if part.len > 0.0 { (cut / part.len) as f32 } else { 0.0 };
            let from = c.from + (c.to - c.from) * along;
            let rate = if c.looping { fps * timing.speed as f32 } else { ((c.to - from) / real.max(1e-9) as f32).max(0.0) };
            segments.push(Segment { start: at, len: real, slot: c.slot, from, rate, looping: c.looping });
        }
        lens.push(real);
        at += real;
    }
    (segments, lens)
}

/// The frames of `slot` in `kit` (0 when it has none).
fn frames_in(kit: &Kit, slot: Slot) -> u16 {
    kit.clips.frames_of(slot)
}

/// Plan `shape` for an actor with `kit`: its performance announced at `arrival`, its blow landing
/// `budget` seconds later ([`super::cue::impact_delay`]).
pub fn plan(kit: &Kit, shape: Shape, arrival: f64, budget: f64) -> Performance {
    let fps = if kit.clips.fps > 0.0 { kit.clips.fps } else { super::model::BATTLE_CLIP_FPS };
    let fps64 = f64::from(fps);
    match shape {
        Shape::Melee { reach, strike } => strike_plan(kit, reach, strike, true, arrival, budget, fps),
        Shape::Stand { strike } => strike_plan(kit, 0.0, strike, false, arrival, budget, fps),
        Shape::Blow { reach, melee } => match kit.blow(melee) {
            Some(chain) => {
                let reach = if chain.closes_in && melee { reach } else { reach.min(party::HOP) };
                chain_plan(kit, chain, reach, arrival, budget, fps)
            }
            // No script to play: the lunge of an actor with no clip (a long-range blow stays where it is).
            None => strike_plan(kit, if melee { reach } else { 0.0 }, None, melee, arrival, budget, fps),
        },
        Shape::Cast => {
            let [begin, hold, end] = kit.cast;
            let Some(begin) = begin else {
                // No clips: the spell still breaks at the impact; the caster stays as it is.
                return Performance { impact: arrival + budget, segments: Vec::new(), lunge: None, reach: 0.0, end: arrival + budget };
            };
            let n = frames_in(kit, begin);
            let pre = f64::from(n.saturating_sub(1)) / fps64;
            let timing = fit(arrival, budget, pre);
            let parts = [Part { len: pre, clip: Some(ClipRun { slot: begin, from: 0.0, to: f32::from(n.saturating_sub(1)), looping: false }) }];
            let (mut segments, _) = warp(&parts, timing, fps);
            let mut at = timing.impact;
            // The spell breaks: the caster holds the pose for a moment, then ends it.
            match (hold, end) {
                (Some(hold), end) => {
                    segments.push(Segment { start: at, len: CAST_HOLD, slot: hold, from: 0.0, rate: fps, looping: true });
                    at += CAST_HOLD;
                    if let Some(end) = end {
                        let len = (f64::from(frames_in(kit, end).saturating_sub(1)) / fps64).clamp(MIN_BACK, MAX_BACK);
                        segments.push(Segment { start: at, len, slot: end, from: 0.0, rate: ((f64::from(frames_in(kit, end).saturating_sub(1))) / len) as f32, looping: false });
                        at += len;
                    }
                }
                (None, _) => {
                    segments.push(Segment { start: at, len: CAST_HOLD, slot: begin, from: f32::from(n.saturating_sub(1)), rate: 0.0, looping: false });
                    at += CAST_HOLD;
                }
            }
            Performance { impact: timing.impact, segments, lunge: None, reach: 0.0, end: at }
        }
        Shape::Guard => {
            let at = arrival;
            let segments = kit.defend.map(|slot| Segment { start: at, len: GUARD_HOLD, slot, from: 0.0, rate: fps, looping: true }).into_iter().collect();
            Performance { impact: arrival, segments, lunge: None, reach: 0.0, end: at + GUARD_HOLD }
        }
    }
}

/// A blow: run up (if `melee` and there is ground to cover), the clip up to its impact frame, the rest
/// of it, the way back.
fn strike_plan(kit: &Kit, reach: f32, strike: Option<Strike>, melee: bool, arrival: f64, budget: f64, fps: f32) -> Performance {
    let fps64 = f64::from(fps);
    let out_nat = if melee && reach > 1.0 { (0.14 + f64::from(reach) / 12_000.0).clamp(MIN_LUNGE_OUT.into(), MAX_LUNGE_OUT.into()) } else { 0.0 };
    // The clip: how long to its impact frame, how long after.
    let (n, impact_frame, wind, follow) = match strike.filter(|s| frames_in(kit, s.slot) > 0) {
        Some(s) => {
            let n = frames_in(kit, s.slot);
            let last = f32::from(n - 1);
            if n <= HELD_POSE_FRAMES {
                (n, 0.0, 0.0, HOLD_POSE)
            } else {
                let frame = (s.impact.clamp(0.0, 1.0) * last).round();
                (n, frame, f64::from(frame) / fps64, (f64::from(last - frame) / fps64).clamp(MIN_FOLLOW, MAX_FOLLOW))
            }
        }
        None => (0, 0.0, 0.0, super::motion::LUNGE_HOLD.into()),
    };
    let pre = out_nat + wind;
    let timing = fit(arrival, budget, pre);
    let mut parts = Vec::with_capacity(2);
    if melee && out_nat > 0.0 {
        parts.push(Part { len: out_nat, clip: kit.run.map(|slot| ClipRun { slot, from: 0.0, to: 0.0, looping: true }) });
    }
    let strike_slot = strike.map(|s| s.slot).filter(|_| n > 0);
    if wind > 0.0 {
        parts.push(Part { len: wind, clip: strike_slot.map(|slot| ClipRun { slot, from: 0.0, to: impact_frame, looping: false }) });
    }
    let (mut segments, lens) = warp(&parts, timing, fps);
    let out_real = if melee && out_nat > 0.0 { lens.first().copied().unwrap_or(0.0) } else { 0.0 };
    let mut at = timing.impact;
    if let Some(slot) = strike_slot {
        let held = n <= HELD_POSE_FRAMES;
        // A pose of a frame or two is held (two frames flicker, as a gun firing does); a longer clip plays on.
        let (from, rate) = if held { (0.0, fps) } else { (impact_frame, ((f32::from(n - 1) - impact_frame) / follow as f32).max(0.0)) };
        segments.push(Segment { start: at, len: follow, slot, from, rate, looping: held && n > 1 });
    }
    at += follow;
    let mut lunge = None;
    if melee && reach > 1.0 {
        let back = match kit.back {
            Some(slot) => {
                let m = frames_in(kit, slot);
                let len = (f64::from(m.saturating_sub(1)) / fps64).clamp(MIN_BACK, MAX_BACK);
                segments.push(Segment { start: at, len, slot, from: 0.0, rate: (f64::from(m.saturating_sub(1)) / len) as f32, looping: false });
                len
            }
            None => out_nat * 1.2,
        };
        let hop = if kit.run.is_some() { 0.0 } else { PROCEDURAL_HOP };
        // It stands at its target from arriving to the end of the follow-through: the swing up to the
        // impact and the rest of it.
        let hold = (timing.impact - timing.start - out_real).max(0.0) + follow;
        let l = Lunge { start: timing.start, out: out_real, hold, back, hop };
        at += back;
        lunge = Some(l);
    }
    Performance { impact: timing.impact, segments, lunge, reach: if lunge.is_some() { reach } else { 0.0 }, end: at }
}

/// A party member's blow as its script plays it ([`Shape::Blow`]): the approach clips (the actor travels from the one
/// the script moves at), the strike clips up to the tick the blow lands at, then the rest of them and the way back.
/// **Every clip plays once whatever its length**: a two-frame leap or a three-frame recovery is a motion, not a held
/// aim (the 1-to-3 frame rule of [`HELD_POSE_FRAMES`] is for an enemy's poses). Before the impact it is all fitted to
/// the schedule by [`fit`]; the follow-through and the way back are the only part that is not, as for every blow.
fn chain_plan(kit: &Kit, chain: &Chain, reach: f32, arrival: f64, budget: f64, fps: f32) -> Performance {
    let fps64 = f64::from(fps);
    let ticks = |slot: Slot| f64::from(kit.clips.frames_of(slot).max(1));
    let run = |slot: Slot, from: f64, to: f64| Part { len: (to - from) / fps64, clip: Some(ClipRun { slot, from: from as f32, to: to as f32, looping: false }) };
    // Before the impact: the approach, then the strike clips up to the tick the blow lands at; what is left of the
    // strike clips comes after it.
    let mut parts: Vec<Part> = chain.approach.iter().map(|&slot| run(slot, 0.0, ticks(slot))).collect();
    let mut after: Vec<(Slot, f64, f64)> = Vec::new();
    let mut left = f64::from(chain.impact);
    for &slot in &chain.strike {
        let n = ticks(slot);
        if left >= n {
            parts.push(run(slot, 0.0, n));
            left -= n;
        } else if left > 0.0 {
            parts.push(run(slot, 0.0, left));
            after.push((slot, left, n));
            left = 0.0;
        } else {
            after.push((slot, 0.0, n));
        }
    }
    let timing = fit(arrival, budget, parts.iter().map(|p| p.len).sum());
    let (mut segments, lens) = warp(&parts, timing, fps);
    // After the impact: the rest of the strike, over its natural length (within the follow-through's bounds), then
    // the way back.
    let remaining: f64 = after.iter().map(|&(_, a, b)| b - a).sum();
    let follow = if remaining > 0.0 { (remaining / fps64).clamp(MIN_FOLLOW, MAX_FOLLOW) } else { MIN_FOLLOW };
    let mut at = timing.impact;
    for &(slot, a, b) in &after {
        let len = (b - a) / remaining * follow;
        segments.push(Segment { start: at, len, slot, from: a as f32, rate: ((b - a) / len) as f32, looping: false });
        at += len;
    }
    if after.is_empty() {
        // The blow lands on the strike's last frame: hold it through the follow-through.
        if let Some(&slot) = chain.strike.last() {
            segments.push(Segment { start: at, len: follow, slot, from: (ticks(slot) - 1.0) as f32, rate: 0.0, looping: false });
        }
        at += follow;
    }
    let back_ticks: f64 = chain.back.iter().map(|&slot| ticks(slot)).sum();
    let back = if back_ticks > 0.0 { (back_ticks / fps64).clamp(MIN_BACK, MAX_BACK) } else { MIN_BACK };
    let way_back = at;
    for &slot in &chain.back {
        let n = ticks(slot);
        let len = n / back_ticks * back;
        segments.push(Segment { start: at, len, slot, from: 0.0, rate: (n / len) as f32, looping: false });
        at += len;
    }
    at = at.max(way_back + back);
    // The actor travels with the clips from the one the script moves at: it winds up or crouches in place first. The
    // travel never takes less than MIN_TRAVEL (a plan cut short by `fit` still closes in over a moment: it does not
    // jump to its target).
    let lunge = (reach > 1.0).then(|| {
        let first = chain.travel_from.min(chain.approach.len());
        let start = timing.start + lens.iter().take(first).sum::<f64>();
        let out = lens.iter().skip(first).take(chain.approach.len() - first).sum::<f64>().max(MIN_TRAVEL);
        Lunge { start, out, hold: (timing.impact - start - out).max(0.0) + follow, back, hop: 0.0 }
    });
    Performance { impact: timing.impact, segments, lunge, reach: if lunge.is_some() { reach } else { 0.0 }, end: at }
}

/// The item clip with its impact point (the middle of the gesture): the one thrown up to an ally, the one thrown
/// forward to an enemy (slot 10: it was taken for the attack).
pub fn item_strike(kit: &Kit, at_enemy: bool) -> Option<Strike> {
    let slot = if at_enemy { kit.item_at_enemy.or(kit.item) } else { kit.item };
    slot.map(|slot| Strike { slot, impact: 0.5 })
}

#[cfg(test)]
mod tests {
    use super::super::clips::testing::{cloud_like_pack, cloud_like_scripts};
    use super::super::clips::ClipSet;
    use super::*;

    fn near(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    /// The invariant of [`fit`]: the impact is on schedule.
    fn on_schedule(arrival: f64, budget: f64, pre: f64) -> Timing {
        let t = fit(arrival, budget, pre);
        assert!(near(t.impact, arrival + budget), "{arrival} {budget} {pre}: {t:?}");
        assert!(near(t.start + (pre - t.skip) / t.speed, arrival + budget), "{arrival} {budget} {pre}: {t:?}");
        assert!(t.skip >= 0.0 && (MIN_SPEED..=MAX_SPEEDUP).contains(&t.speed) && t.start >= arrival - 1e-9, "{t:?}");
        t
    }

    #[test]
    fn a_plan_is_fitted_so_that_its_impact_always_lands_on_schedule() {
        // Short: slowed to the floor and started late. Just right: played faster. Long: capped, the start skipped.
        let short = on_schedule(10.0, 0.5, 0.15);
        assert!(short.speed == MIN_SPEED && short.start > 10.0 && short.skip == 0.0);
        let fitted = on_schedule(10.0, 0.5, 0.8);
        assert!(near(fitted.speed, 1.6) && fitted.start == 10.0 && fitted.skip == 0.0);
        let long = on_schedule(10.0, 0.6, 3.0);
        assert!(long.speed == MAX_SPEEDUP && long.start == 10.0 && near(long.skip, 3.0 - 0.6 * 3.0));
        // The edges of the bands.
        on_schedule(0.0, 0.5, 0.5 * MIN_SPEED);
        on_schedule(0.0, 0.5, 0.5 * MAX_SPEEDUP);
        on_schedule(0.0, 0.5, 0.0);
        // A grid of everything at once: no combination ever lands late.
        for budget in [0.05, 0.3, 0.45, 0.5, 0.6] {
            for pre in [0.0, 0.01, 0.2, 0.6, 1.0, 2.0, 10.0, 1e6] {
                on_schedule(123.456, budget, pre);
            }
        }
    }

    #[test]
    fn nothing_to_wait_for_happens_now_and_nonsense_numbers_are_made_sane() {
        let t = fit(5.0, 0.0, 0.8);
        assert_eq!((t.start, t.impact), (5.0, 5.0));
        assert!(t.skip >= 0.8 - 1e-9, "all of the plan is skipped: {t:?}");
        for (budget, pre) in [(f64::NAN, 0.5), (0.5, f64::NAN), (-1.0, -1.0), (f64::INFINITY, 1.0)] {
            let t = fit(2.0, budget, pre);
            assert!(t.start.is_finite() && t.speed.is_finite() && t.skip.is_finite() && t.impact.is_finite(), "{budget} {pre}: {t:?}");
            assert!(t.impact <= 2.0 + MAX_IMPACT_DELAY + 1e-9, "{budget} {pre}: never later than the cap");
        }
    }

    /// A 15 fps pack of 16 slots: idle 0, run 2 (looping 6 frames), strike 3 (15 frames), 4 (way back),
    /// a held pose at 5, spell clips at 7..=9.
    fn kit() -> Kit {
        let mut frames = vec![10u16; 16];
        frames[2] = 6;
        frames[3] = 15;
        frames[4] = 10;
        frames[5] = 2;
        frames[7] = 30;
        frames[8] = 6;
        frames[9] = 8;
        Kit {
            clips: ClipSet { travel: vec![0.0; 16], ok: vec![true; 16], frames, fps: 15.0 },
            run: Some(2),
            back: Some(4),
            cast: [Some(7), Some(8), Some(9)],
            defend: Some(6),
            ..Kit::default()
        }
    }

    fn strike(slot: Slot) -> Option<Strike> {
        Some(Strike { slot, impact: 0.5 })
    }

    #[test]
    fn a_melee_blow_runs_up_swings_to_the_impact_frame_follows_through_and_comes_back() {
        let kit = kit();
        let p = plan(&kit, Shape::Melee { reach: 2000.0, strike: strike(3) }, 10.0, 0.5);
        assert!(near(p.impact, 10.5), "{p:?}");
        // The run-up, then the swing up to its impact frame (7 of 14), then the rest, then the way back.
        let slots: Vec<Slot> = p.segments.iter().map(|s| s.slot).collect();
        assert_eq!(slots, [2, 3, 3, 4]);
        let (run, wind, follow, back) = (p.segments[0], p.segments[1], p.segments[2], p.segments[3]);
        assert!(run.looping && near(run.start, 10.0) && near(run.end(), wind.start));
        assert!(near(wind.end(), 10.5) && near(follow.start, 10.5), "the swing reaches its impact frame exactly at the impact");
        // (Frames are single precision.)
        assert!((wind.frame_at(wind.len, 15) - 7.0).abs() < 1e-3 && follow.from == 7.0);
        assert!(near(follow.end(), back.start) && !follow.looping, "the follow-through plays the rest of the clip");
        assert!((follow.frame_at(follow.len, 15) - 14.0).abs() < 1e-3, "and ends on its last frame");
        // The procedural run (translation) matches the clips: out, a hold from arriving to the end of the
        // follow-through (the swing and the rest of it), back.
        let l = p.lunge.expect("a run");
        assert!(near(l.start, 10.0) && near(l.out, run.len) && near(l.hold, wind.len + follow.len) && near(l.back, back.len), "{l:?}");
        assert_eq!(l.hop, 0.0, "a clip runs: no procedural hop");
        assert!(near(p.end, back.end()) && near(l.end(), p.end));
    }

    #[test]
    fn a_blow_is_never_later_than_its_budget_however_long_its_clips_are() {
        // The same actor with a very long swing and a very long run: compressed up to the cap, then skipped.
        let mut kit = kit();
        kit.clips.frames[3] = 120;
        for budget in [0.3, 0.5, 0.6] {
            for reach in [0.0f32, 300.0, 2000.0, 9000.0] {
                let p = plan(&kit, Shape::Melee { reach, strike: strike(3) }, 4.0, budget);
                assert!(near(p.impact, 4.0 + budget), "{budget} {reach}: {p:?}");
                for s in &p.segments {
                    assert!(s.start >= 4.0 - 1e-9 && s.len >= 0.0, "{s:?}");
                }
                // What plays before the impact is over at the impact, in order and never overlapping.
                let before: Vec<&Segment> = p.segments.iter().filter(|s| s.start < p.impact - 1e-9).collect();
                for pair in before.windows(2) {
                    assert!(pair[0].end() <= pair[1].start + 1e-9, "{pair:?}");
                }
                assert!(before.iter().all(|s| s.end() <= p.impact + 1e-9), "{before:?}");
            }
        }
    }

    /// A party member with Cloud's chain: its pack has the lengths of his clips (26: 2 frames, 27: 5, 30: 6, 28: 3,
    /// 29: 4; the hops 14 and 12, the long-range swing 31), its scripts the shape of his.
    fn cloud() -> Kit {
        Kit::party(cloud_like_pack(), &cloud_like_scripts())
    }

    #[test]
    fn a_party_blow_plays_its_scripts_clips_in_order_and_the_blow_lands_on_the_schedules_tick() {
        let kit = cloud();
        let p = plan(&kit, Shape::Blow { reach: 2000.0, melee: true }, 10.0, 0.5);
        assert!(near(p.impact, 10.5), "{p:?}");
        // The crouch, the run, the chop up to the tick the blow lands on, the rest of the chop, the recovery, the way back.
        let slots: Vec<Slot> = p.segments.iter().map(|s| s.slot).collect();
        assert_eq!(slots, [26, 27, 30, 30, 28, 29]);
        assert!(p.segments.iter().all(|s| !s.looping), "every clip plays once, a 2-frame crouch and a 3-frame recovery included");
        assert!(p.segments.iter().all(|s| s.rate > 0.0), "and none is held: {:?}", p.segments);
        let (crouch, run, wind, follow, recover, back) = (p.segments[0], p.segments[1], p.segments[2], p.segments[3], p.segments[4], p.segments[5]);
        // Back to back before the impact, the first starting at the arrival: 2 + 5 + 2 ticks of 15 a second (0.6 s) in 0.5 s.
        assert!(near(crouch.start, 10.0) && near(crouch.end(), run.start) && near(run.end(), wind.start), "{:?}", p.segments);
        assert!(near(wind.end(), 10.5) && near(follow.start, 10.5), "the chop's first tick ends at the impact");
        assert!((wind.frame_at(wind.len, 6) - 2.0).abs() < 1e-3 && follow.from == 2.0, "the blow lands two ticks into the chop");
        // After it: the rest of the chop (frames 1 to 6), then the recovery, then the way back.
        assert!(near(follow.end(), recover.start) && near(recover.end(), back.start));
        assert!((back.frame_at(back.len, 4) - 3.0).abs() < 1e-3, "the way back ends on its last frame");
        // The actor travels from the run (the move to the target comes after the crouch), holds there through the
        // follow-through and comes back with the way-back clip.
        let l = p.lunge.expect("it runs up");
        assert!(near(l.start, run.start) && near(l.out, run.len), "{l:?} {run:?}");
        assert!(near(l.hold, wind.len + follow.len + recover.len) && near(l.back, back.len), "{l:?}");
        assert!(near(l.end(), p.end) && near(p.end, back.end()), "{l:?}");
        assert_eq!(l.hop, 0.0, "the clips are the motion: no procedural hop");
    }

    #[test]
    fn a_long_range_blow_only_hops_and_a_plan_that_fit_cuts_into_never_jumps_to_its_target() {
        let kit = cloud();
        // Hop forward (14 ticks), the shot 12 ticks in: 26 ticks (1.7 s) in 0.5 s is past the speed cap, so the start of the hop is skipped.
        let p = plan(&kit, Shape::Blow { reach: 3000.0, melee: false }, 4.0, 0.5);
        assert!(near(p.impact, 4.5));
        let slots: Vec<Slot> = p.segments.iter().map(|s| s.slot).collect();
        assert_eq!(slots, [3, 31, 31, 4]);
        let hop = p.segments[0];
        assert!(hop.from > 0.0 && near(hop.start, 4.0), "the crouch of the hop was skipped, the rest plays from the arrival: {hop:?}");
        let l = p.lunge.expect("it hops forward");
        // It closes in over the hop's own length (never less than the minimum) from the very start: its offset is continuous.
        assert!(near(l.start, 4.0) && l.out >= MIN_TRAVEL - 1e-9 && near(l.out, hop.len), "{l:?} {hop:?}");
        // A hop covers the hop and no more, wherever the target is: the offset is bounded by it (see `motion`).
        assert_eq!(l.hop, 0.0);
        // A plan cut so deep that nothing of the approach is left still closes in over a moment, from the first instant.
        let mut deep = cloud();
        if let Some(chain) = deep.shoot.as_mut() {
            chain.impact = 31;
        }
        deep.clips.frames[31] = 200;
        let p = plan(&deep, Shape::Blow { reach: 3000.0, melee: false }, 4.0, 0.5);
        let l = p.lunge.expect("it hops forward");
        assert!(near(l.start, 4.0) && near(l.out, MIN_TRAVEL), "no jump: {l:?}");
        assert!(p.segments.iter().all(|s| s.slot != 3), "the whole hop was skipped");
    }

    #[test]
    fn a_party_blows_lunge_is_the_hops_length_for_a_script_that_does_not_move_and_the_whole_way_for_one_that_does() {
        let kit = cloud();
        // `Shape::Blow`'s reach is capped at the hop for a chain that does not close in (see `plan`): the offset of the actor
        // at the end of its approach is that, checked through the director in `motion`; here the plan's lunge only exists
        // when there is ground to cover.
        assert!(plan(&kit, Shape::Blow { reach: 0.0, melee: true }, 1.0, 0.5).lunge.is_none());
        assert!(plan(&kit, Shape::Blow { reach: 1.0, melee: true }, 1.0, 0.5).lunge.is_none());
        assert!(plan(&kit, Shape::Blow { reach: 2000.0, melee: true }, 1.0, 0.5).lunge.is_some());
    }

    #[test]
    fn a_party_blows_follow_through_and_way_back_are_bounded_and_the_impact_is_never_late_whatever_the_clips() {
        // The same chain with very long clips (200 frames: 13 seconds each).
        let mut kit = cloud();
        for slot in [26, 27, 30, 28, 29, 3, 31, 4] {
            kit.clips.frames[slot] = 200;
        }
        for (melee, budget) in [(true, 0.3), (true, 0.5), (false, 0.6), (false, 0.0)] {
            for reach in [0.0f32, 400.0, 5000.0] {
                let p = plan(&kit, Shape::Blow { reach, melee }, 9.0, budget);
                assert!(near(p.impact, 9.0 + budget), "{melee} {budget} {reach}: {p:?}");
                let before: Vec<&Segment> = p.segments.iter().filter(|s| s.start < p.impact - 1e-9).collect();
                for pair in before.windows(2) {
                    assert!(pair[0].end() <= pair[1].start + 1e-9, "{pair:?}");
                }
                assert!(before.iter().all(|s| s.end() <= p.impact + 1e-9 && s.start >= 9.0 - 1e-9), "{before:?}");
                // Nothing after the impact is longer than the follow-through and the way back allow, together.
                assert!(p.end - p.impact <= MAX_FOLLOW + MAX_BACK + 1e-9, "{p:?}");
                for s in &p.segments {
                    assert!(s.len.is_finite() && s.len >= 0.0 && s.rate.is_finite() && s.from.is_finite(), "{s:?}");
                }
            }
        }
    }

    #[test]
    fn a_party_blow_without_scripts_runs_up_and_back_with_the_shared_clips_and_swings_nothing() {
        // No script gives a blow: the actor runs up with the run clip (slot 7) and comes back with the hop back (slot 4)
        // and plays no swing, never the item throw (slot 10): the procedural lunge, on schedule.
        let bare = Kit::party(cloud_like_pack(), &[]);
        let p = plan(&bare, Shape::Blow { reach: 2500.0, melee: true }, 7.0, 0.5);
        let slots: Vec<Slot> = p.segments.iter().map(|s| s.slot).collect();
        assert_eq!(slots, [7, 4]);
        assert!(near(p.impact, 7.5) && p.lunge.is_some());
        // A long-range blow stays where it is.
        let shot = plan(&bare, Shape::Blow { reach: 2500.0, melee: false }, 7.0, 0.5);
        assert!(shot.segments.is_empty() && shot.lunge.is_none() && near(shot.impact, 7.5));
        // An actor with nothing at all hops up to its target.
        let none = plan(&Kit::none(), Shape::Blow { reach: 2500.0, melee: true }, 7.0, 0.5);
        assert!(none.segments.is_empty() && none.lunge.expect("it runs").hop == PROCEDURAL_HOP);
    }

    #[test]
    fn an_item_is_thrown_up_to_an_ally_and_forward_to_an_enemy() {
        let kit = cloud();
        assert_eq!((item_strike(&kit, false).map(|s| s.slot), item_strike(&kit, true).map(|s| s.slot)), (Some(9), Some(10)));
        // A model with the gesture for allies only throws that one at enemies too.
        let only = Kit { item_at_enemy: None, ..kit };
        assert_eq!(item_strike(&only, true).map(|s| s.slot), Some(9));
        assert_eq!(item_strike(&Kit::none(), true), None);
    }

    #[test]
    fn a_stand_in_place_blow_has_no_run_and_a_held_pose_is_held_and_flickers() {
        let kit = kit();
        let p = plan(&kit, Shape::Stand { strike: strike(5) }, 3.0, 0.55);
        assert!(p.lunge.is_none() && p.segments.len() == 1, "{p:?}");
        let held = p.segments[0];
        assert!(near(held.start, 3.55) && near(held.len, HOLD_POSE) && held.looping, "a two-frame pose flickers through the hold: {held:?}");
        assert!(near(p.end, 3.55 + HOLD_POSE));
        // One frame is simply held.
        let mut one = kit.clone();
        one.clips.frames[5] = 1;
        let p = plan(&one, Shape::Stand { strike: strike(5) }, 3.0, 0.55);
        assert!(!p.segments[0].looping);
    }

    #[test]
    fn without_clips_the_run_is_the_procedural_lunge_and_still_lands_on_schedule() {
        let bare = Kit::none();
        let p = plan(&bare, Shape::Melee { reach: 2500.0, strike: None }, 7.0, 0.5);
        assert!(p.segments.is_empty() && near(p.impact, 7.5));
        let l = p.lunge.expect("a run");
        assert_eq!(l.hop, PROCEDURAL_HOP);
        assert!(near(l.start + l.out, p.impact), "the run reaches its target at the impact: {l:?}");
        assert!(l.out <= f64::from(MAX_LUNGE_OUT) / MIN_SPEED + 1e-9);
        // A blow already at its target does not run.
        assert!(plan(&bare, Shape::Melee { reach: 0.0, strike: None }, 7.0, 0.5).lunge.is_none());
    }

    #[test]
    fn a_cast_begins_to_the_impact_holds_and_ends_and_a_long_begin_is_cut_not_late() {
        let mut kit = kit();
        let p = plan(&kit, Shape::Cast, 20.0, 0.6);
        assert!(near(p.impact, 20.6));
        let slots: Vec<Slot> = p.segments.iter().map(|s| s.slot).collect();
        assert_eq!(slots, [7, 8, 9], "begin, hold, end");
        // 29 frames at 15 fps is 1.93 s: at the speed cap it covers 1.8 s of it, so the start is skipped and
        // the begin clip starts part-way and still ends at the impact on its last frame.
        let begin = p.segments[0];
        assert!(begin.from > 0.0 && near(begin.end(), 20.6), "{begin:?}");
        assert!((begin.frame_at(begin.len, 30) - 29.0).abs() < 1e-3);
        assert!(p.segments[1].looping && near(p.segments[1].start, 20.6));
        assert!(near(p.end, p.segments[2].end()));
        // A shorter begin is not skipped at all.
        kit.clips.frames[7] = 6;
        let short = plan(&kit, Shape::Cast, 20.0, 0.6);
        assert!(short.segments[0].from == 0.0 && near(short.segments[0].end(), 20.6));
        // Without a hold clip the last frame is held; without any cast clip nothing plays but the impact stays.
        kit.cast = [Some(7), None, None];
        let held = plan(&kit, Shape::Cast, 20.0, 0.6);
        assert_eq!(held.segments.len(), 2);
        assert!(held.segments[1].rate == 0.0);
        kit.cast = [None; 3];
        let bare = plan(&kit, Shape::Cast, 20.0, 0.6);
        assert!(bare.segments.is_empty() && near(bare.impact, 20.6));
    }

    #[test]
    fn a_guard_holds_its_pose_for_a_while_and_without_a_clip_nothing_plays() {
        let kit = kit();
        let p = plan(&kit, Shape::Guard, 1.0, 0.0);
        assert_eq!(p.segments.len(), 1);
        assert!(near(p.segments[0].len, GUARD_HOLD) && near(p.end, 1.0 + GUARD_HOLD));
        assert!(plan(&Kit::none(), Shape::Guard, 1.0, 0.0).segments.is_empty());
    }

    #[test]
    fn a_segments_frame_runs_from_its_start_to_its_end_and_loops_or_stops() {
        let s = Segment { start: 0.0, len: 1.0, slot: 3, from: 2.0, rate: 4.0, looping: false };
        assert_eq!((s.frame_at(0.0, 10), s.frame_at(1.0, 10), s.frame_at(100.0, 10), s.frame_at(-1.0, 10)), (2.0, 6.0, 9.0, 2.0));
        let l = Segment { looping: true, ..s };
        assert!((l.frame_at(2.0, 6) - 4.0).abs() < 1e-6, "10 frames in a clip of 6 is frame 4");
        assert_eq!(Segment { rate: 0.0, ..s }.frame_at(5.0, 10), 2.0);
        // A clip with no frames never produces nonsense.
        assert_eq!(s.frame_at(1.0, 0), 0.0);
    }
}
