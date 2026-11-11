//! Renderer-free scene presentation: the same actor/effect reducer for playback and validation.
//!
//! This is the current port's scene-time policy, not a claim of original field scheduling.
//! Drawing, audio delivery and gameplay authority are outside this module.

pub mod clip;
pub mod shake;
pub mod stage;
pub use clip::ClipFrame;
pub use shake::ShakeState;
pub use stage::{Actor, CameraClock, CameraState, Cast, CastCheckpoint, Counts, Height, MAX_CAST_CHECKPOINT_BYTES};

use super::{AnimHow, MoveHow, Stage};

/// The exact model-loader slot needed by a supported presentation event. Resolving it remains
/// the source's job; the reducer never invents a fallback model or animation.
pub fn animation_slot(event: &Stage) -> Option<(u8, u8)> {
    match event {
        Stage::Anim { entity, animation, .. } => Some((*entity, *animation)),
        Stage::Move { entity, how: MoveHow::Walk, animation: Some(slot @ (1 | 2)), .. } => Some((*entity, *slot)),
        _ => None,
    }
}

/// Apply resolved source metadata through the single live presentation reducer. Movement
/// phase is supplied only for the caller's verified adjacent ordinary MOVE continuation.
pub fn apply_stage(cast: &mut Cast, event: &Stage, late: f32, movement_phase: f32,
    clip: Option<&str>, height: Height<'_>) {
    // Presentation uses the script's own field model and loaded clip bounds: normal-rate
    // Holds/Once, plus full loops and fresh full Holds at normal or doubled speed.
    // Other forms clear an old override; the stage handles supported standing-loop rate changes.
    if let Stage::Anim { entity, animation, how: AnimHow::Loop, first: None, last: None, speed: speed @ (16 | 32), divisor: 1 } = event {
        if let Some(clip) = clip {
            cast.apply_loop(*entity, *animation, clip, late, *speed == 32);
            return;
        }
    }
    if let Stage::Anim { entity, how: AnimHow::Hold, first: None, last: None, speed: speed @ (16 | 32), divisor: 1, .. } = event {
        if let Some(clip) = clip {
            cast.apply_hold_full(*entity, clip, late, *speed == 32);
            return;
        }
    }
    if let Stage::Anim { entity, how: AnimHow::Hold, first: Some(first), last: Some(last), speed: speed @ (16 | 32), divisor: 1, .. } = event {
        if first <= last {
            if let Some(clip) = clip {
                if first == last { cast.apply_hold(*entity, clip, *first); }
                else if *speed == 32 { cast.apply_hold_range_doubled(*entity, clip, *first, *last, late); }
                else { cast.apply_hold_range(*entity, clip, *first, *last, late); }
                return;
            }
        }
    }
    if let Stage::Anim { entity, animation, how: AnimHow::Once, first: None, last: None, speed: speed @ (16 | 32), divisor: 1 } = event {
        if let Some(clip) = clip {
            let applied = if *speed == 32 { cast.apply_once_full_doubled(*entity, *animation, clip, late) }
                else { cast.apply_once_full(*entity, *animation, clip, late) };
            if applied { return; }
        }
    }
    if let Stage::Anim { entity, animation, how: AnimHow::Once, first: Some(first), last: Some(last), speed: 16, divisor: 1 } = event {
        if first <= last {
            if let Some(clip) = clip {
                if cast.apply_once(*entity, *animation, clip, *first, *last, late) { return; }
            }
        }
    }
    cast.apply(event, late, height);
    if let Stage::Move { entity, how: MoveHow::Walk, animation: Some(1 | 2), .. } = event {
        if let Some(clip) = clip {
            cast.apply_move_animation(*entity, clip, movement_phase);
        }
    }
}
