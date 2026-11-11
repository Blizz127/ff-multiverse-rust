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
mod default_names;
mod fade;
mod jump;
mod offset;
mod play;
mod replay_identity;
pub mod train_arrival;
mod vm;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod background_tests;
#[cfg(test)]
mod palette_tests;
#[cfg(test)]
mod offset_tests;
#[cfg(test)]
mod native_vm_tests;
#[cfg(test)]
mod native_outcome_tests;
#[cfg(test)]
mod scratch_vm_tests;
#[cfg(test)]
mod scratch_scene_tests;

pub use data::*;
pub use fade::{FadeBlend, FadeColor, FadeOp, FadeState};
pub use jump::{jump_frames, JumpMotion};
pub use offset::OffsetMotion;
pub use play::{auto_witnesses, auto_witnesses_with_resources, cast_places, cast_places_with_resources, run_section_to_end, run_section_to_end_with_resources, run_to_end, run_to_end_with_resources, run_to_end_with_resources_strict, trigger_npc, Ground, Outcome, SceneError, ScenePlay, TriggerNpc, Writes, ANNOUNCED_STATE_OPS, FULL_RESTORE_OPS, SKIPPED_STATE_OPS};
pub use vm::{AnimHow, Beat, CameraOp, GilOp, SceneResources, Resources, ResourceOutcome, Limits, MoveHow, SceneJump, SceneVars, Stage, Stats, Then, Timed, Vm, Where, Window, FRAMES_PER_SECOND};

pub use play::{BattleCheckpoint, BattleResolution, BattleResume, WindowCheckpoint, WindowAck, SceneInput, SceneBoundary, SceneSnapshot, ReplayIdentity, REPLAY_VERSION, replay_scene, replay_scene_to, with_trigger_position};
pub use play::{replay_scene_with_scratch, replay_scene_to_with_scratch, run_to_end_with_scratch_strict, run_section_to_end_with_scratch_strict};
pub use train_arrival::{TrainArrival, TrainArrivalEntry, TrainArrivalReady, TrainArrivalPose,
    TRAIN_ARRIVAL_LANDING, replay_train_arrival, train_arrival_ready};
