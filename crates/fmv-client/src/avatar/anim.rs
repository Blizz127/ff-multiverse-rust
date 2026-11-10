//! Which clip an avatar plays and where in it.
//!
//! An avatar's `AnimState` picks one of three clips ([`Character::animation`]): idle (a standing
//! pose), walk and run (loops of 14 to 30 frames that cover two steps). The walk and run loops
//! advance with the *distance* the avatar covers, which here is time while it moves:
//! [`Animator`] keeps one phase (0..1 around the loop) per avatar and moves it on only while the
//! avatar walks or runs, at the field's frame rate ([`FIELD_FPS`]) divided by the clip's length.
//! Because both loops are two-step cycles, the phase carries across a switch between walking and
//! running, so the legs do not jump. Stopping holds the phase.
//!
//! The idle clip is a still pose: measured on the owned archive, the idle clips of all eight
//! playable characters have no motion at all (three of them have two frames, which are identical;
//! `real_idle_clips_measured` in the avatar screenshot tests prints the numbers), so idle shows
//! frame 0 and nothing is played. NPCs have their own authored idle loops (for example trapped
//! Jessie's 45-frame clip), so [`Animator::advance_npc`] advances those at the field frame rate.
//!
//! Remote avatars are driven by what the server reports (`AnimState` in the snapshot); the local
//! player by the keys (walk with Shift, run otherwise, see `crate::window`). A frame position is
//! fractional: [`fmv_ff7::FieldAnimation::pose_at`] blends the two neighbouring frames, so the
//! clips play smoothly at the display's frame rate whatever it is.

use fmv_core::AnimState;
use std::collections::HashMap;

/// The field module's frame rate, which is the rate the clips were authored at.
pub const FIELD_FPS: f32 = fmv_ff7::FF7_FIELD_FRAMES_PER_SECOND;

/// A scene-controlled frame. Loops wrap; held poses must name a real frame of the loaded clip.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ClipFrame {
    Loop(f32),
    Hold(u8),
    /// A forward range that holds its last frame rather than looping past it.
    HoldRange { first: u8, last: u8, elapsed: f32 },
    /// Play from frame zero through the loaded clip's actual last frame, then hold it.
    HoldFull { elapsed: f32 },
}

impl ClipFrame {
    pub fn in_clip(self, frames: usize) -> Option<f32> {
        if frames == 0 { return None; }
        match self {
            Self::Loop(frame) => Some(if frame.is_finite() { frame.max(0.0).rem_euclid(frames as f32) } else { 0.0 }),
            Self::Hold(frame) => (usize::from(frame) < frames).then_some(f32::from(frame)),
            Self::HoldRange { first, last, elapsed } => {
                if first > last || usize::from(last) >= frames { return None; }
                let elapsed = if elapsed.is_finite() { elapsed.max(0.0) } else { 0.0 };
                Some(f32::from(first) + elapsed.min(f32::from(last - first)))
            }
            Self::HoldFull { elapsed } => {
                let elapsed = if elapsed.is_finite() { elapsed.max(0.0) } else { 0.0 };
                Some(elapsed.min((frames - 1) as f32))
            }
        }
    }
}

/// Entities remembered (a frame's drawable players are bounded far below this; the cap only keeps
/// a hostile stream of entity ids from growing the table).
const MAX_TRACKED: usize = 1024;

/// The clip that shows `state`, as the index of a [`Clip`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Clip {
    Idle,
    Walk,
    Run,
}

impl Clip {
    pub fn for_state(state: AnimState) -> Self {
        match state {
            AnimState::Idle => Clip::Idle,
            AnimState::Walk => Clip::Walk,
            AnimState::Run => Clip::Run,
        }
    }
}

/// Who an animation belongs to: a remote entity or the local player.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Who(pub u64);

#[derive(Debug, Clone, Copy, Default)]
struct Track {
    /// Position around the walk/run loop, 0 up to (excluding) 1.
    phase: f32,
    /// The frame this track was last advanced in (for dropping what has left).
    seen: u64,
    /// Restart an NPC idle when its clip length changes with its story phase.
    npc_frames: Option<usize>,
}

/// One phase per avatar. Call [`Animator::begin_frame`] once per drawn frame, then
/// [`Animator::advance`] for every avatar that is drawn.
#[derive(Debug, Default)]
pub struct Animator {
    tracks: HashMap<Who, Track>,
    frame: u64,
}

/// A frame position inside a clip.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Playback {
    pub clip: Clip,
    /// Fractional frame (0 up to the clip's frame count): what `pose_at` takes.
    pub frame: f32,
}

impl Animator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a drawn frame: whoever is not advanced in it is forgotten at the next call (an
    /// avatar that left, or one drawn again later starts its loop afresh).
    pub fn begin_frame(&mut self) {
        let current = self.frame;
        self.tracks.retain(|_, t| t.seen == current);
        self.frame = current.wrapping_add(1);
        if self.tracks.len() > MAX_TRACKED {
            self.tracks.clear();
        }
    }

    /// Move `who`'s animation on by `dt` seconds in `state` and say where in its clip it is.
    /// `frames` is the length of the clip `state` shows ([`fmv_ff7::FieldAnimation::frame_count`]).
    /// A non-finite or negative `dt` counts as no time passing.
    pub fn advance(&mut self, who: Who, state: AnimState, dt: f32, frames: usize) -> Playback {
        self.advance_inner(who, state, dt, frames, false)
    }

    /// Advance an NPC's authored standing loop without treating it as a walking player.
    pub fn advance_npc(&mut self, who: Who, dt: f32, frames: usize) -> Playback {
        self.advance_inner(who, AnimState::Idle, dt, frames, true)
    }

    fn advance_inner(&mut self, who: Who, state: AnimState, dt: f32, frames: usize, loop_idle: bool) -> Playback {
        let dt = if dt.is_finite() && dt > 0.0 { dt } else { 0.0 };
        let frames = frames.max(1);
        let track = self.tracks.entry(who).or_default();
        track.seen = self.frame;
        if loop_idle && track.npc_frames != Some(frames) {
            track.phase = 0.0;
            track.npc_frames = Some(frames);
        }
        let clip = Clip::for_state(state);
        let frame = match clip {
            // A still pose (see the module docs).
            Clip::Idle if !loop_idle => 0.0,
            _ => {
                track.phase = (track.phase + dt * FIELD_FPS / frames as f32).rem_euclid(1.0);
                track.phase * frames as f32
            }
        };
        // Floating-point rounding must never push the frame to the clip's end: `pose_at` wraps
        // anyway, but a position below the frame count is the documented contract.
        let frame = if frame >= frames as f32 { 0.0 } else { frame };
        Playback { clip, frame }
    }

    /// Avatars currently tracked.
    #[cfg(test)]
    pub fn tracked(&self) -> usize {
        self.tracks.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_holds_use_the_loaded_endpoint_without_truncating_large_clips() {
        for elapsed in [0.0, 0.5, 1000.0, -1.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let frame = ClipFrame::HoldFull { elapsed };
            assert_eq!(frame.in_clip(0), None);
            assert_eq!(frame.in_clip(1), Some(0.0));
        }
        for elapsed in [-1.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(ClipFrame::HoldFull { elapsed }.in_clip(300), Some(0.0));
        }
        assert_eq!(ClipFrame::HoldFull { elapsed: 256.5 }.in_clip(300), Some(256.5));
        for elapsed in [299.0, 300.0, 1000.0, f32::MAX] {
            assert_eq!(ClipFrame::HoldFull { elapsed }.in_clip(300), Some(299.0));
        }
    }

    #[test]
    fn npc_idle_loops_wrap_and_reset_for_a_new_clip_while_player_idle_stays_still() {
        let mut animator = Animator::new();
        animator.begin_frame();
        let npc = Who(2);
        let first = animator.advance_npc(npc, 0.5, 45);
        assert_eq!(first.clip, Clip::Idle);
        assert!((first.frame - 15.0).abs() < 0.001);
        let wrapped = animator.advance_npc(npc, 1.1, 45);
        assert!((wrapped.frame - 3.0).abs() < 0.001);
        assert_eq!(animator.advance(Who(1), AnimState::Idle, 0.5, 45).frame, 0.0);
        assert_eq!(animator.advance_npc(npc, 0.0, 1).frame, 0.0);
        assert_eq!(animator.advance_npc(npc, 0.0, 45).frame, 0.0, "a changed idle restarts");
        for dt in [f32::NAN, f32::INFINITY, -1.0, 0.0] {
            assert_eq!(animator.advance_npc(npc, dt, 45).frame, 0.0);
        }
        assert_eq!(animator.tracked(), 2, "same bounded track table");
    }

    const WHO: Who = Who(7);

    #[test]
    fn each_state_shows_its_own_clip() {
        assert_eq!(Clip::for_state(AnimState::Idle), Clip::Idle);
        assert_eq!(Clip::for_state(AnimState::Walk), Clip::Walk);
        assert_eq!(Clip::for_state(AnimState::Run), Clip::Run);
        let mut a = Animator::new();
        a.begin_frame();
        assert_eq!(a.advance(WHO, AnimState::Walk, 0.0, 30).clip, Clip::Walk);
        assert_eq!(a.advance(WHO, AnimState::Run, 0.0, 15).clip, Clip::Run);
        assert_eq!(a.advance(WHO, AnimState::Idle, 0.0, 1).clip, Clip::Idle);
    }

    #[test]
    fn walking_plays_the_loop_at_the_field_frame_rate_and_wraps() {
        let mut a = Animator::new();
        a.begin_frame();
        // One second of a 30-frame walk is 30 frames: exactly one loop.
        let mut last = a.advance(WHO, AnimState::Walk, 0.0, 30);
        assert_eq!(last.frame, 0.0);
        for _ in 0..10 {
            last = a.advance(WHO, AnimState::Walk, 0.05, 30);
        }
        // 0.5 s in: frame 15 (give or take float noise).
        assert!((last.frame - 15.0).abs() < 1e-3, "{}", last.frame);
        for _ in 0..10 {
            last = a.advance(WHO, AnimState::Walk, 0.05, 30);
        }
        assert!(last.frame < 1e-2 || (last.frame - 30.0).abs() < 1e-2, "back at the start of the loop: {}", last.frame);
        assert!(last.frame < 30.0);
        // A run clip of 15 frames takes half a second per loop.
        let mut b = Animator::new();
        b.begin_frame();
        b.advance(WHO, AnimState::Run, 0.0, 15);
        let p = b.advance(WHO, AnimState::Run, 0.25, 15);
        assert!((p.frame - 7.5).abs() < 1e-3, "{}", p.frame);
    }

    #[test]
    fn the_phase_carries_across_walk_and_run_and_is_held_while_standing() {
        let mut a = Animator::new();
        a.begin_frame();
        a.advance(WHO, AnimState::Walk, 0.0, 30);
        let walk = a.advance(WHO, AnimState::Walk, 0.5, 30); // half way round: frame 15 of 30
        assert!((walk.frame - 15.0).abs() < 1e-3);
        // Switching to the 15-frame run clip keeps the same point of the cycle: frame 7.5 of 15.
        let run = a.advance(WHO, AnimState::Run, 0.0, 15);
        assert!((run.frame - 7.5).abs() < 1e-3, "{}", run.frame);
        // Standing still holds it...
        let idle = a.advance(WHO, AnimState::Idle, 1.0, 1);
        assert_eq!((idle.clip, idle.frame), (Clip::Idle, 0.0));
        let again = a.advance(WHO, AnimState::Run, 0.0, 15);
        assert!((again.frame - 7.5).abs() < 1e-3, "{}", again.frame);
    }

    #[test]
    fn idle_is_a_still_pose_whatever_the_length_of_its_clip() {
        // The real idle clips of two frames are two identical frames: nothing to play.
        for frames in [1usize, 2, 7] {
            let mut a = Animator::new();
            a.begin_frame();
            for _ in 0..100 {
                let p = a.advance(WHO, AnimState::Idle, 0.1, frames);
                assert_eq!((p.clip, p.frame), (Clip::Idle, 0.0), "{frames} frames");
            }
        }
    }

    #[test]
    fn nonsense_time_never_breaks_a_frame_position() {
        let mut a = Animator::new();
        a.begin_frame();
        for dt in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -1.0, 0.0, 1.0e9] {
            for state in [AnimState::Idle, AnimState::Walk, AnimState::Run] {
                for frames in [0usize, 1, 2, 15, 30] {
                    let p = a.advance(WHO, state, dt, frames);
                    assert!(p.frame.is_finite() && p.frame >= 0.0 && p.frame < frames.max(1) as f32, "{dt} {state:?} {frames}: {}", p.frame);
                }
            }
        }
    }

    #[test]
    fn avatars_that_stop_being_drawn_are_forgotten_and_the_table_is_bounded() {
        let mut a = Animator::new();
        a.begin_frame();
        a.advance(Who(1), AnimState::Walk, 0.3, 30);
        a.advance(Who(2), AnimState::Walk, 0.3, 30);
        assert_eq!(a.tracked(), 2);
        // Next frame only #2 is drawn: #1 is dropped, and starts from the beginning if it returns.
        a.begin_frame();
        a.advance(Who(2), AnimState::Walk, 0.0, 30);
        a.begin_frame();
        assert_eq!(a.tracked(), 1);
        a.advance(Who(1), AnimState::Walk, 0.0, 30);
        assert_eq!(a.advance(Who(1), AnimState::Walk, 0.0, 30).frame, 0.0);
        // A flood of distinct ids cannot grow the table without bound.
        for id in 0..(MAX_TRACKED as u64 * 3) {
            a.advance(Who(id), AnimState::Walk, 0.01, 30);
        }
        a.begin_frame();
        assert!(a.tracked() <= MAX_TRACKED, "{}", a.tracked());
    }
}
