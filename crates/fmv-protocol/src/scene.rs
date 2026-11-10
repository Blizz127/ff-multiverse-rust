//! Story scenes on the wire: a player's OK on a scene's trigger starts a scene for that player; party members in range
//! watch it; its lines are the game's own, read by every client from its own files.
//!
//! These messages add variants to [`ClientMsg`](crate::ClientMsg) and [`ServerMsg`](crate::ServerMsg).
//! Protocol v18 rejects older peers, which cannot decode them or participate in a scene hold.
//!
//! # What crosses the wire, and what does not
//!
//! Only ids, numbers and choices: a scene's id, a seed, the story it plays against, who takes part, which window the
//! owner has moved past and what it answered. **No dialogue, no model, no script byte ever travels.** Every client builds
//! the same scene from its OWN copy of the field (`fmv_ff7::script::scene`, deterministic for the same scene, story and
//! seed), so the lines of the owner and of the watchers are the same lines in the same order.
//!
//! # The flow
//!
//! ```text
//! (S -> C  AreaSnapshot { npcs: [.. a scene NPC, id >= SCENE_NPC_ID_BASE ..] })       the trigger is on offer
//! C -> S  Interact { npc: Some(SCENE_NPC_ID_BASE + n) }                              OK next to it
//! S -> C  SceneStart { run, scene, seed, progress, owner, participants, you }        every participant
//!         ... each client plays the scene locally; the player is held (no moves, no encounters, no OK) ...
//! C -> S  SceneStep { run, step, answer }                                            the owner moved past window `step`
//! S -> C  SceneStep { run, step, answer }                                            every watcher (not the owner)
//! C -> S  SceneEnd { run, skipped }                                                   the owner finished or skipped
//! S -> C  SceneEnded { run, outcome }                                                every participant: control returns
//! ```
//!
//! * A scene is held by the SERVER: it chooses the seed, decides who may watch (party members of the owner that stand in
//!   the same instance, in range, not in a battle, at most [`MAX_SCENE_WATCHERS`]), numbers the run, enforces time limits
//!   and ends it whatever a client says or fails to say.
//! * The **owner** drives. Its windows open and close on its own keys; each window it moves past is reported once, in
//!   order (`step` counts from 0), with the absolute line number it chose when the window asked. The server checks the
//!   order and relays it to the watchers, whose clients open each window when the owner moves past the one before.
//! * A **watcher** only watches (it may leave with `SceneLeave`, which frees it at once and nobody else is affected).
//! * When the scene ends, [`ServerMsg::SceneEnded`] says how, and every participant's client hands control and camera
//!   back. A story write the scene's script makes is applied by the server ONCE, from the answers the owner reported, and
//!   told with the ordinary [`ServerMsg::StoryUpdate`].
//! * [`ClientMsg::SceneDebugPreview`] (refused unless the server runs with `--debug-scene-preview`) puts the player in a
//!   scene's field at the scene's own story moment, on a made-up story that is never saved.

use fmv_core::EntityId;
use serde::{Deserialize, Serialize};

/// NPC ids from this one up are scene trigger NPCs the server adds to an area's cast (`AreaSnapshot::npcs`): the id
/// `SCENE_NPC_ID_BASE + n` is the trigger of the `n`-th scene of the game's scene list for that field. The area's own
/// NPCs are numbered from 0 and an area has at most [`crate::MAX_NPCS_PER_AREA`] of them, so the ranges never meet.
pub const SCENE_NPC_ID_BASE: u32 = 1_000_000;

/// Most watchers a scene has besides its owner (a party has three members).
pub const MAX_SCENE_WATCHERS: usize = 2;

/// Longest scene id a message carries (characters); a longer one is refused.
pub const MAX_SCENE_ID_CHARS: usize = 64;

/// Most windows a scene is taken to have (what `SceneStep::step` may count up to): a step past it is refused.
pub const MAX_SCENE_STEPS: u32 = 256;

/// One run of a scene, numbered by the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SceneRunId(pub u64);

/// What a player is in a scene.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SceneRole {
    /// The player that started it: its keys drive the windows and its answers are the scene's.
    Owner,
    /// A party member that watches it: it sees what the owner sees, in step.
    Watcher,
}

/// Somebody who stands in a scene as a witness: its entity, and which of the scene's witness spots it stands on (0 is
/// the owner's).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SceneParticipant {
    pub entity: EntityId,
    pub spot: u8,
}

/// How a scene ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SceneOutcome {
    /// The owner's client played it to its end.
    Finished,
    /// The owner skipped it.
    Skipped,
    /// The owner said nothing for too long (or the scene ran past its time limit): the server ended it.
    TimedOut,
    /// The owner is gone (it disconnected, a battle took it, its party changed): the scene is dropped, and nothing it
    /// would have written is applied.
    Aborted,
}

/// Why a scene was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SceneRefusal {
    /// The server does not run scene previews (`--debug-scene-preview` is off), or has no scenes.
    Unavailable,
    /// The server has no scene of that id.
    UnknownScene,
    /// The player cannot start one now (in a battle, in transit, already in a scene).
    Busy,
    /// The scene is not on offer to this player here (wrong field or story moment).
    NotHere,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClientMsg, ServerMsg};

    fn golden<T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug>(value: T, json: &str) {
        assert_eq!(serde_json::to_string(&value).unwrap(), json);
        assert_eq!(serde_json::from_str::<T>(json).unwrap(), value);
    }

    #[test]
    fn the_scene_messages_have_the_wire_shape_documented_here() {
        golden(ClientMsg::SceneStep { run: SceneRunId(7), step: 3, answer: Some(2) }, r#"{"SceneStep":{"run":7,"step":3,"answer":2}}"#);
        golden(ClientMsg::SceneStep { run: SceneRunId(7), step: 0, answer: None }, r#"{"SceneStep":{"run":7,"step":0}}"#);
        golden(ClientMsg::SceneEnd { run: SceneRunId(7), skipped: true }, r#"{"SceneEnd":{"run":7,"skipped":true}}"#);
        golden(ClientMsg::SceneLeave { run: SceneRunId(7) }, r#"{"SceneLeave":{"run":7}}"#);
        golden(ClientMsg::SceneDebugPreview { scene: "ff7/mds7pb_1/tifa-bar".into() }, r#"{"SceneDebugPreview":{"scene":"ff7/mds7pb_1/tifa-bar"}}"#);
        golden(
            ServerMsg::SceneStart {
                run: SceneRunId(7),
                scene: "ff7/mds7pb_1/tifa-bar".into(),
                seed: 41,
                progress: vec![1, 72, 0],
                owner: EntityId(3),
                participants: vec![SceneParticipant { entity: EntityId(3), spot: 0 }, SceneParticipant { entity: EntityId(4), spot: 1 }],
                you: SceneRole::Watcher,
            },
            concat!(
                r#"{"SceneStart":{"run":7,"scene":"ff7/mds7pb_1/tifa-bar","seed":41,"progress":[1,72,0],"owner":3,"#,
                r#""participants":[{"entity":3,"spot":0},{"entity":4,"spot":1}],"you":"Watcher"}}"#
            ),
        );
        golden(ServerMsg::SceneStep { run: SceneRunId(7), step: 3, answer: Some(2) }, r#"{"SceneStep":{"run":7,"step":3,"answer":2}}"#);
        golden(ServerMsg::SceneEnded { run: SceneRunId(7), outcome: SceneOutcome::TimedOut }, r#"{"SceneEnded":{"run":7,"outcome":"TimedOut"}}"#);
        golden(ServerMsg::SceneRefused { reason: SceneRefusal::Unavailable }, r#"{"SceneRefused":{"reason":"Unavailable"}}"#);
    }

    #[test]
    fn scenes_require_the_new_protocol() {
        assert_eq!(crate::PROTOCOL_VERSION, 18);
    }

    #[test]
    fn scene_npc_ids_never_meet_an_areas_own() {
        assert!(u32::try_from(crate::MAX_NPCS_PER_AREA).unwrap() < SCENE_NPC_ID_BASE);
    }
}
