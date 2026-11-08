//! Snapshot interpolation for remote entities.
//!
//! Remote entities are drawn `INTERP_DELAY_S` in the past, blended between the two server
//! samples that bracket that moment. If the newest sample is older than the render time the
//! entity simply holds its last pose (no extrapolation).
//!
//! The server only sends entities that changed, so a standing entity has no samples for as long
//! as it stands. When it moves again the two bracketing samples are far apart in time; blending
//! across that whole gap would put the entity almost at the new position the moment the sample
//! arrives. A gap longer than `MAX_CONTINUOUS_GAP_S` is therefore treated as "held, then moved":
//! the old pose is kept and only the last `GLIDE_S` is interpolated, exactly as a steady update
//! stream would have drawn the step.
//!
//! All blending is done in `f64` and converted back, so finite inputs of any size stay finite.

use fmv_core::{AnimState, Pos};
use std::collections::VecDeque;
use std::f32::consts::PI;
use std::f64::consts::{PI as PI64, TAU as TAU64};

/// Remote entities are rendered this far behind "now" so two samples normally bracket the time.
pub const INTERP_DELAY_S: f64 = 0.100;
/// Samples kept per entity (bounded memory; ~0.3 s at 25 Hz).
const MAX_SAMPLES: usize = 8;
/// Samples further apart than this (five ticks at the server's 20 Hz) are not continuous motion.
const MAX_CONTINUOUS_GAP_S: f64 = 0.25;
/// How long the move after a long gap is blended over (about one server tick).
const GLIDE_S: f64 = 0.05;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pose {
    pub pos: Pos,
    pub facing: f32,
    pub anim: AnimState,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    /// Local receive time in seconds (monotonic, caller-defined origin).
    pub t: f64,
    pub pose: Pose,
}

#[derive(Debug, Default, Clone)]
pub struct SnapshotBuffer {
    samples: VecDeque<Sample>,
}

impl SnapshotBuffer {
    /// Add a sample. Timestamps must increase: an older sample is dropped and a sample with the
    /// same timestamp replaces the newest one. The buffer never grows beyond `MAX_SAMPLES`.
    pub fn push(&mut self, s: Sample) {
        if !s.t.is_finite() {
            return;
        }
        if let Some(last) = self.samples.back_mut() {
            if s.t < last.t {
                return;
            }
            if s.t == last.t {
                *last = s;
                return;
            }
        }
        if self.samples.len() == MAX_SAMPLES {
            self.samples.pop_front();
        }
        self.samples.push_back(s);
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    #[cfg(test)]
    pub fn latest(&self) -> Option<&Sample> {
        self.samples.back()
    }

    /// Pose at `render_t`: linear blend between the bracketing samples (see the module docs for
    /// long gaps), held at the first/last sample outside the buffered range. `None` only when the
    /// buffer is empty.
    pub fn sample(&self, render_t: f64) -> Option<Pose> {
        let first = self.samples.front()?;
        let mut prev = first;
        for next in self.samples.iter().skip(1) {
            if render_t <= next.t {
                // Timestamps are strictly increasing, so the gap is positive.
                let gap = next.t - prev.t;
                // Where the blend starts: at the previous sample for continuous motion, one
                // glide before the new sample after a long gap (the old pose is held until then).
                let start = if gap > MAX_CONTINUOUS_GAP_S { next.t - GLIDE_S } else { prev.t };
                let span = next.t - start;
                let a = if span > 0.0 {
                    ((render_t - start) / span).clamp(0.0, 1.0) as f32
                } else {
                    1.0
                };
                return Some(lerp_pose(&prev.pose, &next.pose, a));
            }
            prev = next;
        }
        Some(prev.pose)
    }
}

/// `a + (b - a) * t` computed in `f64`: `b - a` cannot overflow for finite `f32` inputs, and for
/// `t` in `0..=1` the result lies between `a` and `b`, so it is finite as well.
fn lerp_f32(a: f32, b: f32, t: f32) -> f32 {
    let (a, b, t) = (f64::from(a), f64::from(b), f64::from(t));
    (a + (b - a) * t) as f32
}

pub fn lerp_pose(a: &Pose, b: &Pose, t: f32) -> Pose {
    Pose {
        pos: Pos::new(
            lerp_f32(a.pos.x, b.pos.x, t),
            lerp_f32(a.pos.y, b.pos.y, t),
            lerp_f32(a.pos.z, b.pos.z, t),
        ),
        facing: lerp_angle(a.facing, b.facing, t),
        anim: if t < 0.5 { a.anim } else { b.anim },
    }
}

/// Wrap an angle (radians) into `[-pi, pi]`. Angles already in range are returned unchanged and a
/// non-finite angle becomes 0, so the result is always a plain, finite angle.
pub fn normalize_angle(a: f32) -> f32 {
    if !a.is_finite() {
        return 0.0;
    }
    if a.abs() <= PI {
        return a;
    }
    let mut r = f64::from(a).rem_euclid(TAU64); // [0, tau)
    if r > PI64 {
        r -= TAU64;
    }
    (r as f32).clamp(-PI, PI)
}

/// Interpolate between two angles (radians) along the shortest arc. Computed in `f64`, so even
/// huge finite angles give a finite result instead of overflowing into NaN, and the result is
/// wrapped into `[-pi, pi]` (crossing the +-pi seam would otherwise leave it slightly outside).
pub fn lerp_angle(a: f32, b: f32, t: f32) -> f32 {
    let (a, b, t) = (f64::from(a), f64::from(b), f64::from(t));
    let mut d = (b - a) % TAU64;
    if d > PI64 {
        d -= TAU64;
    } else if d < -PI64 {
        d += TAU64;
    }
    normalize_angle((a + d * t) as f32)
}

/// The time at which remote entities should be sampled when the local clock reads `now`.
pub fn render_time(now: f64) -> f64 {
    now - INTERP_DELAY_S
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pose(x: f32, y: f32, facing: f32, anim: AnimState) -> Pose {
        Pose {
            pos: Pos::new(x, y, 0.0),
            facing,
            anim,
        }
    }
    fn sample(t: f64, x: f32, y: f32) -> Sample {
        Sample {
            t,
            pose: pose(x, y, 0.0, AnimState::Walk),
        }
    }
    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn blends_between_the_bracketing_samples() {
        let mut b = SnapshotBuffer::default();
        b.push(sample(1.00, 0.0, 0.0));
        b.push(sample(1.10, 10.0, 20.0));
        let p = b.sample(1.05).unwrap();
        assert!(near(p.pos.x, 5.0) && near(p.pos.y, 10.0), "{p:?}");
        let p = b.sample(1.075).unwrap();
        assert!(near(p.pos.x, 7.5) && near(p.pos.y, 15.0), "{p:?}");
    }

    #[test]
    fn holds_outside_the_buffered_range_and_with_one_sample() {
        let mut b = SnapshotBuffer::default();
        assert!(b.sample(1.0).is_none());
        b.push(sample(1.0, 3.0, 4.0));
        assert!(near(b.sample(0.0).unwrap().pos.x, 3.0));
        assert!(near(b.sample(99.0).unwrap().pos.y, 4.0));
        b.push(sample(1.1, 13.0, 4.0));
        assert!(
            near(b.sample(0.5).unwrap().pos.x, 3.0),
            "before the first sample: hold the first"
        );
        assert!(
            near(b.sample(5.0).unwrap().pos.x, 13.0),
            "after the last sample: hold, never extrapolate"
        );
    }

    #[test]
    fn picks_the_right_segment_among_many() {
        let mut b = SnapshotBuffer::default();
        for i in 0..5 {
            b.push(sample(f64::from(i) * 0.05, (i * 10) as f32, 0.0));
        }
        // Between t=0.10 (x=20) and t=0.15 (x=30).
        assert!(near(b.sample(0.125).unwrap().pos.x, 25.0));
        // Exactly on a sample.
        assert!(near(b.sample(0.05).unwrap().pos.x, 10.0));
    }

    #[test]
    fn render_delay_samples_the_past() {
        // Updates every 50 ms moving 10 units each; at now = 0.30 we draw the state of t = 0.20.
        let mut b = SnapshotBuffer::default();
        for i in 0..7 {
            b.push(sample(f64::from(i) * 0.05, (i * 10) as f32, 0.0));
        }
        let p = b.sample(render_time(0.30)).unwrap();
        assert!(near(p.pos.x, 40.0), "{p:?}");
        assert!((render_time(1.0) - 0.9).abs() < 1e-12);
    }

    #[test]
    fn angle_interpolation_takes_the_shortest_arc() {
        let a = 170.0_f32.to_radians();
        let b = (-170.0_f32).to_radians();
        let mid = lerp_angle(a, b, 0.5);
        assert!(near(mid.cos(), -1.0) && mid.sin().abs() < 1e-4, "mid {mid}");
        assert!(near(lerp_angle(0.0, 1.0, 0.25), 0.25));
        assert!(near(lerp_angle(1.0, 1.0, 0.7), 1.0));
        // Large angles (unwrapped facing values) must not blow up.
        let r = lerp_angle(0.0, 7.0 * PI, 1.0);
        assert!(near(r.cos(), -1.0));
    }

    #[test]
    fn facing_and_anim_blend_with_position() {
        // A 100 ms span is continuous motion (see `long_gaps_...` for the other case).
        let mut b = SnapshotBuffer::default();
        b.push(Sample {
            t: 0.0,
            pose: pose(0.0, 0.0, 0.0, AnimState::Idle),
        });
        b.push(Sample {
            t: 0.1,
            pose: pose(1.0, 0.0, 1.0, AnimState::Walk),
        });
        assert_eq!(b.sample(0.025).unwrap().anim, AnimState::Idle);
        assert_eq!(b.sample(0.075).unwrap().anim, AnimState::Walk);
        assert!(near(b.sample(0.05).unwrap().facing, 0.5));
    }

    /// The server only sends changed entities, so a standing peer has no samples. Its first step
    /// after standing must glide in like any other step instead of popping to the new position.
    #[test]
    fn long_gaps_hold_the_old_pose_then_glide_like_a_steady_stream() {
        let step = 24.0;
        // Idle at x = 0 since t = 0; the first step (+24) arrives at t = 10.
        let mut idle_then_step = SnapshotBuffer::default();
        idle_then_step.push(Sample {
            t: 0.0,
            pose: pose(0.0, 0.0, 0.0, AnimState::Idle),
        });
        idle_then_step.push(Sample {
            t: 10.0,
            pose: pose(step, 0.0, 0.0, AnimState::Walk),
        });
        // The same step as a steady 50 ms stream would have delivered it.
        let mut steady = SnapshotBuffer::default();
        steady.push(Sample {
            t: 9.95,
            pose: pose(0.0, 0.0, 0.0, AnimState::Idle),
        });
        steady.push(Sample {
            t: 10.0,
            pose: pose(step, 0.0, 0.0, AnimState::Walk),
        });
        // `now` runs from the arrival of the step to 100 ms after it (render delay).
        for now in [10.0, 10.02, 10.05, 10.06, 10.075, 10.09, 10.1, 10.2] {
            let r = render_time(now);
            let (a, b) = (idle_then_step.sample(r).unwrap(), steady.sample(r).unwrap());
            assert!(
                near(a.pos.x, b.pos.x),
                "now {now}: {} (idle gap) vs {} (steady)",
                a.pos.x,
                b.pos.x
            );
        }
        // At the moment the step arrives the peer is still drawn where it stood...
        assert!(near(idle_then_step.sample(render_time(10.0)).unwrap().pos.x, 0.0));
        // ...it is halfway through the glide 75 ms later and has arrived after 100 ms.
        assert!(near(idle_then_step.sample(render_time(10.075)).unwrap().pos.x, step / 2.0));
        assert!(near(idle_then_step.sample(render_time(10.1)).unwrap().pos.x, step));
        // A pause just under the threshold is still ordinary continuous motion.
        let mut b = SnapshotBuffer::default();
        b.push(sample(1.0, 0.0, 0.0));
        b.push(sample(1.0 + MAX_CONTINUOUS_GAP_S, 25.0, 0.0));
        assert!(near(b.sample(1.0 + MAX_CONTINUOUS_GAP_S / 2.0).unwrap().pos.x, 12.5));
    }

    /// The server accepts and relays any finite facing/position; interpolation must not turn
    /// huge finite values into NaN or infinity.
    #[test]
    fn huge_finite_values_stay_finite() {
        assert!(lerp_angle(3.0e38, -3.0e38, 0.5).is_finite());
        assert!(lerp_angle(f32::MAX, f32::MIN, 0.0).is_finite());
        assert!(lerp_angle(f32::MIN, f32::MAX, 1.0).is_finite());
        let a = pose(3.0e38, -3.0e38, 3.0e38, AnimState::Idle);
        let b = pose(-3.0e38, 3.0e38, -3.0e38, AnimState::Walk);
        for t in [0.0, 0.25, 0.5, 1.0] {
            let p = lerp_pose(&a, &b, t);
            assert!(
                p.pos.is_finite() && p.facing.is_finite(),
                "t {t}: {p:?}"
            );
        }
        assert_eq!(lerp_pose(&a, &b, 0.0).pos.x, 3.0e38, "t = 0 returns the first pose exactly");
        assert_eq!(lerp_pose(&a, &b, 1.0).pos.x, -3.0e38, "t = 1 returns the second pose exactly");
        // Normalisation: in-range values are untouched, everything else lands in [-pi, pi].
        assert_eq!(normalize_angle(1.25), 1.25);
        assert_eq!(normalize_angle(-PI), -PI);
        assert_eq!(normalize_angle(f32::NAN), 0.0);
        assert_eq!(normalize_angle(f32::INFINITY), 0.0);
        for raw in [3.0e38_f32, -3.0e38, f32::MAX, f32::MIN, 7.0 * PI, -9.5, 100.0, 1.0e10] {
            let n = normalize_angle(raw);
            assert!(n.is_finite() && n.abs() <= PI, "{raw} -> {n}");
        }
        assert!(near(normalize_angle(7.0 * PI).cos(), -1.0));
        assert!(near(normalize_angle(2.0 * PI + 0.5), 0.5));
        // Interpolating across the +-pi seam stays a plain angle (3.0 -> -3.0 passes through pi).
        for t in [0.0, 0.2, 0.5, 0.8, 1.0] {
            let f = lerp_angle(3.0, -3.0, t);
            assert!(f.abs() <= PI, "t {t}: {f}");
        }
        assert!(near(lerp_angle(3.0, -3.0, 0.5).abs(), PI));
    }

    #[test]
    fn buffer_is_bounded_and_ordered() {
        let mut b = SnapshotBuffer::default();
        for i in 0..100 {
            b.push(sample(f64::from(i), i as f32, 0.0));
        }
        assert_eq!(b.len(), MAX_SAMPLES);
        assert!(near(b.latest().unwrap().pose.pos.x, 99.0));
        // Out-of-order and non-finite samples are ignored; an equal timestamp replaces.
        b.push(sample(50.0, -1.0, 0.0));
        b.push(sample(f64::NAN, -1.0, 0.0));
        assert!(near(b.latest().unwrap().pose.pos.x, 99.0));
        b.push(sample(99.0, 7.0, 0.0));
        assert!(near(b.latest().unwrap().pose.pos.x, 7.0));
        assert_eq!(b.len(), MAX_SAMPLES);
    }
}
