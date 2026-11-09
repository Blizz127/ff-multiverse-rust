//! What a fighter's statuses do to how its body is drawn: a colour factor on the whole model (Poison's purple pulse, a
//! sleeper's dim blue, the grey of a stopped or paralysed body, Haste's brightness...), a droop for a sleeper, and a body
//! that stops moving (Stop, Paralysis: its idle loop is held on one frame).
//!
//! Presentation only, like the rest of the scene: the engine's state says which statuses a fighter has
//! ([`super::BattleScene::update`] copies them from the replayed engine every frame, and the window then replaces them with the
//! panels' own, which change when the performance that changes them lands: [`super::BattleScene::show_statuses`]) and this
//! module turns them into numbers for [`super::BattleScene::frame`]. Pure: the statuses, the scene clock and the fighter's place in the cast
//! in, a [`Body`] out; no GPU, no random.
//!
//! The factors multiply (a poisoned sleeper is dim, blue and purple at once) and are held between [`MIN_RGB`] and
//! [`MAX_RGB`], so that no pile of statuses turns a body black or white. A factor above 1 brightens (see
//! `crate::avatar::gpu::Tint`).

use fmv_ff7_battle::status::{Status, StatusSet};

/// The darkest and brightest a colour factor gets.
pub const MIN_RGB: f32 = 0.40;
pub const MAX_RGB: f32 = 1.35;

/// How a body is drawn this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Body {
    /// The factor on the model's colours (`[1.0; 3]`: as it is).
    pub rgb: [f32; 3],
    /// The factor on the model's height (a sleeper droops).
    pub squash: f32,
    /// The idle loop is held on one frame (Stop, Paralysis).
    pub frozen: bool,
}

impl Body {
    /// A body with no status on it.
    pub const PLAIN: Body = Body { rgb: [1.0; 3], squash: 1.0, frozen: false };
}

/// The body of fighter `i` of the cast with `set` at scene time `now` (seconds). Two fighters with the same statuses
/// pulse out of step (`i` shifts the phase).
pub fn of(set: StatusSet, now: f64, i: usize) -> Body {
    if set.is_empty() {
        return Body::PLAIN;
    }
    let phase = now + i as f64 * 0.73;
    let pulse = |speed: f64| (0.5 + 0.5 * (phase * speed).sin()) as f32;
    let mut rgb = [1.0f32; 3];
    let mut mul = |f: [f32; 3]| {
        for (c, f) in rgb.iter_mut().zip(f) {
            *c *= f;
        }
    };
    let (mut squash, mut frozen) = (1.0, false);
    if set.contains(Status::Poison) {
        // A sickly purple that breathes: between a faint and a strong tint.
        let p = pulse(3.0);
        mul([0.86 - 0.10 * p, 0.66 - 0.06 * p, 0.98]);
    }
    if set.contains(Status::Sleep) {
        // Dim and blue, slumped, breathing slowly.
        mul([0.62, 0.68, 0.92]);
        squash = 0.90 + 0.015 * pulse(1.4);
    }
    if set.contains(Status::Stop) {
        mul([0.66, 0.68, 0.74]);
        frozen = true;
    }
    if set.contains(Status::Paralysis) {
        // Stiff and sallow, with a flicker of the current that holds it.
        mul([0.98, 0.94 - 0.08 * pulse(9.0), 0.66]);
        frozen = true;
    }
    if set.contains(Status::Slow) {
        mul([0.84, 0.80, 0.94]);
    }
    if set.contains(Status::Haste) {
        mul([1.08, 1.12 + 0.05 * pulse(5.0), 1.18]);
    }
    if set.contains(Status::Darkness) {
        mul([0.58, 0.58, 0.62]);
    }
    if set.contains(Status::Regen) {
        mul([0.90, 1.08 + 0.06 * pulse(2.2), 0.92]);
    }
    if set.contains(Status::Barrier) {
        mul([0.92, 1.02, 1.16]);
    }
    if set.contains(Status::MBarrier) {
        mul([1.12, 0.94, 1.12]);
    }
    if set.contains(Status::Fury) {
        mul([1.18, 0.82, 0.80]);
    }
    if set.contains(Status::Sadness) {
        mul([0.82, 0.86, 1.06]);
    }
    for c in &mut rgb {
        *c = c.clamp(MIN_RGB, MAX_RGB);
    }
    Body { rgb, squash, frozen }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(list: &[Status]) -> StatusSet {
        list.iter().copied().collect()
    }

    #[test]
    fn a_body_with_no_status_is_drawn_as_it_is() {
        assert_eq!(of(StatusSet::EMPTY, 12.3, 4), Body::PLAIN);
        // What the rules do not play (and a set of nothing the scene knows) changes nothing either.
        assert_eq!(of(set(&[Status::Confusion, Status::Frog]), 1.0, 0), Body { rgb: [1.0; 3], squash: 1.0, frozen: false });
    }

    #[test]
    fn poison_pulses_purple_and_a_sleeper_droops_dim_and_blue() {
        let (low, high) = (of(set(&[Status::Poison]), 0.0, 0), of(set(&[Status::Poison]), std::f64::consts::PI / 6.0, 0));
        for body in [low, high] {
            assert!(body.rgb[2] > body.rgb[0] && body.rgb[0] > body.rgb[1], "purple: {:?}", body.rgb);
            assert!(!body.frozen && body.squash == 1.0);
        }
        assert_ne!(low.rgb, high.rgb, "it breathes");
        let asleep = of(set(&[Status::Sleep]), 0.0, 0);
        assert!(asleep.squash < 0.95 && asleep.squash > 0.85, "slumped: {}", asleep.squash);
        assert!(asleep.rgb[0] < asleep.rgb[2] && asleep.rgb.iter().all(|c| *c < 1.0), "dim and blue: {:?}", asleep.rgb);
        assert!(!asleep.frozen, "a sleeper still breathes");
    }

    #[test]
    fn a_hold_freezes_the_body_and_the_others_do_not() {
        for held in [Status::Stop, Status::Paralysis] {
            assert!(of(set(&[held]), 3.0, 1).frozen, "{held}");
        }
        for free in [Status::Poison, Status::Sleep, Status::Slow, Status::Haste, Status::Silence, Status::Regen, Status::Darkness] {
            assert!(!of(set(&[free]), 3.0, 1).frozen, "{free}");
        }
    }

    #[test]
    fn haste_brightens_and_slow_and_darkness_dim() {
        let rgb = |s: Status| of(set(&[s]), 0.0, 0).rgb;
        assert!(rgb(Status::Haste).iter().all(|c| *c > 1.0));
        assert!(rgb(Status::Slow).iter().all(|c| *c < 1.0));
        assert!(rgb(Status::Darkness).iter().all(|c| *c < 0.7));
        // Silence has no look of its own: it is the menu's.
        assert_eq!(of(set(&[Status::Silence]), 0.0, 0), Body::PLAIN);
    }

    #[test]
    fn a_pile_of_statuses_stays_between_the_darkest_and_the_brightest_and_in_step_with_the_clock() {
        let everything: StatusSet = Status::ALL.into_iter().filter(|s| s.is_modelled()).collect();
        for t in [0.0, 0.31, 1.7, 40.0] {
            for i in 0..6 {
                let body = of(everything, t, i);
                assert!(body.rgb.iter().all(|c| (MIN_RGB..=MAX_RGB).contains(c) && c.is_finite()), "{:?}", body.rgb);
                assert!(body.squash.is_finite() && body.squash > 0.8 && body.squash <= 1.0);
            }
        }
        // Two fighters with the same status do not pulse together, and the same fighter at the same time is the same.
        let poison = set(&[Status::Poison]);
        assert_ne!(of(poison, 0.0, 0).rgb, of(poison, 0.0, 1).rgb);
        assert_eq!(of(poison, 2.5, 3), of(poison, 2.5, 3));
    }
}
