//! Story scenes: a game's own scripted conversations and cutscenes, played from the owner's files for a player who
//! stands in them as a witness.
//!
//! **Verified on owned FF7 Steam Edition (en) on 2026-11-09; other editions are unverified.**
//!
//! The pieces:
//!
//! * [`data`]: a scene as DATA (ids, names and numbers: the field, the trigger, the cast, where the witness stands).
//!   The proof scene is [`proof_scene`].
//! * [`vm`]: a bounded, read-only interpreter of the field-script opcodes a scene uses (windows, requests, waits,
//!   places, walks, turns, animations, visibility, the camera). It runs the scripts a script requests as threads on a
//!   virtual clock and says, beat by beat, what the stage shows and which window opens. See its docs for how a scene
//!   ends and what is skipped (and counted).
//! * [`ScenePlay`]: one run of a scene in one field for one player's story: the cast, the ground and the speakers, the
//!   story writes ([`Writes`]) and the headless run the server makes ([`run_to_end`]).
//!
//! # What this is not
//!
//! It is not the engine: the interpreter reads the field's scripts and shows their results, it does not decide what
//! the world becomes. It never touches shared state, never gives items, never moves the party to another field. A
//! scene is played locally by whoever watches it and reports its writes to the server, which applies them once.
//!
//! # Honest limits
//!
//! * A window pauses the whole scene (the engine pauses only the script that opened it).
//! * Durations (a walk's length, an animation's) are estimates from the move speed and the clip range, used for the
//!   order of events and for pacing; the picture may finish a clip sooner or later.
//! * Only the opcodes of the proof scene are carried out; a script that uses others skips and counts them
//!   ([`Stats`]).

mod data;
mod play;
mod vm;

#[cfg(test)]
mod tests;

pub use data::*;
pub use play::{run_to_end, trigger_npc, Ground, Outcome, SceneError, ScenePlay, TriggerNpc, Writes};
pub use vm::{AnimHow, Beat, CameraOp, Limits, MoveHow, SceneVars, Stage, Stats, Then, Timed, Vm, Where, Window, FRAMES_PER_SECOND};
