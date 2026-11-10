//! Story scenes on the wire: a player's OK on a scene's trigger starts a scene for that player; party members in range
//! watch it; its lines are the game's own, read by every client from its own files.
//!
//! These messages add variants to [`ClientMsg`](crate::ClientMsg) and [`ServerMsg`](crate::ServerMsg).
//! Protocol v20 requires explicit continuation metadata, and v19 introduced the starting gil
//! snapshot. Older peers are refused rather than losing a held continuation or taking another branch.
//!
//! # What crosses the wire, and what does not
//!
//! Only ids, numbers and choices: a scene's id, a seed, the story and starting gil it plays against, who takes part, which window the
//! owner has moved past and what it answered. **No dialogue, no model, no script byte ever travels.** Every client builds
//! the same scene from its OWN copy of the field (`fmv_ff7::script::scene`, deterministic for the same scene, story, gil and
//! seed), so the lines of the owner and of the watchers are the same lines in the same order.
//!
//! # The flow
//!
//! ```text
//! (S -> C  AreaSnapshot { npcs: [.. a scene NPC, id >= SCENE_NPC_ID_BASE ..] })       the trigger is on offer
//! C -> S  Interact { npc: Some(SCENE_NPC_ID_BASE + n) }                              OK next to it
//! S -> C  SceneStart { run, scene, seed, progress, gil, replay, owner, participants, you } every participant
//!         ... each client plays the scene locally; the player is held (no moves, no encounters, no OK) ...
//! C -> S  SceneStep { run, step, answer }                                            the owner moved past window `step`
//! S -> C  SceneStep { run, step, answer }                                            every watcher (not the owner)
//! C -> S  SceneBattleReady { run, ordinal, step }                                    stage reached a battle boundary
//!         ... server saves the boundary and frozen combat setup ...
//! S -> C  SceneBattleStarted { run, ordinal, battle_id }, EncounterStart { .. }       scene stays held
//!         ... existing battle messages; server saves the outcome before acknowledging it ...
//! S -> C  BattleEnd { .. }, SceneBattleResumed { run, ordinal, battle_id, progress, gil }
//!         ... or SceneGameOver { run, ordinal, battle_id }; owner may send SceneRetry { run } ...
//! C -> S  SceneEnd { run, skipped }                                                   the owner finished or skipped
//! S -> C  SceneEnded { run, outcome }                                                every participant: control returns
//! ```
//!
//! * A scene is held by the SERVER: it chooses the seed, decides who may watch (party members of the owner that stand in
//!   the same instance, in range, not in a battle, at most [`MAX_SCENE_WATCHERS`]), numbers the run and enforces its
//!   lifecycle. A durable combat checkpoint remains reserved across disconnection. The server sends the same starting `gil` to every participant;
//!   watchers must use that snapshot, never their own balances, when a script reads money.
//! * The **owner** drives. Its windows open and close on its own keys; each window it moves past is reported once, in
//!   order (`step` counts from 0), with the absolute line number it chose when the window asked. The server checks the
//!   order and relays it to the watchers, whose clients open each window when the owner moves past the one before.
//! * A **watcher** watches the story, and may participate in a continuation's co-op fight. `SceneLeave` can free a
//!   watcher in an ordinary scene; it cannot discard a reserved battle checkpoint. An owner sends `SceneLeave` when
//!   it cannot play an ordinary scene; that run is aborted without applying story effects.
//! * When the scene ends, [`ServerMsg::SceneEnded`] says how, and every participant's client hands control and camera
//!   back. A story write the scene's script makes is applied by the server ONCE, from the answers the owner reported, and
//!   told with the ordinary [`ServerMsg::StoryUpdate`].
//! * [`ClientMsg::SceneDebugPreview`] (refused unless the server runs with `--debug-scene-preview`) puts the player in a
//!   scene's field at the scene's own story moment, on a made-up story that is never saved.
//! * A continuation replays only accepted window acknowledgements and durably resolved victories.
//!   Its source fingerprint binds the local script and interpretation rules. Rebuilding this history
//!   emits no presentation, commands or rewards; it reconstructs the next held boundary. A battle
//!   result never comes from a client. Game over preserves the checkpoint and holds control.

use fmv_core::{EntityId, Pos};
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

/// Maximum accepted window/battle events carried by one continuation. Frame limits apply too.
pub const MAX_SCENE_REPLAY_EVENTS: usize = 128;
/// Maximum bytes in an adapter's exact window or battle identity. Identities contain no script data.
pub const MAX_SCENE_IDENTITY_BYTES: usize = 64;

/// The server-accepted pose that activated a line scene. The adapter binds it into the scene
/// definition before fingerprinting; recovery must not substitute a canned witness position.
/// Finite field coordinates are required here; the host additionally checks its map's bounds.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct SceneOrigin {
    pub pos: Pos,
    pub facing: f32,
    pub triangle: Option<u32>,
}

impl SceneOrigin {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !self.pos.is_finite() || !self.facing.is_finite() { return Err("scene origin is not finite"); }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for SceneOrigin {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            pos: Pos,
            facing: f32,
            #[serde(deserialize_with = "crate::required_option")]
            triangle: Option<u32>,
        }
        let wire = Wire::deserialize(deserializer)?;
        let origin = Self { pos: wire.pos, facing: wire.facing, triangle: wire.triangle };
        origin.validate().map_err(serde::de::Error::custom)?;
        Ok(origin)
    }
}

/// The accepted prefix of a durable scene, reconstructed from SceneStart's original story, gil and
/// seed. An empty prefix still opts into continuation playback. The host verifies every identity
/// against this source; shape validation alone does not authorize advancing a script or awarding loot.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SceneReplay {
    pub source: [u8; 32],
    /// Explicit null for a scene whose definition has no dynamic activation pose.
    pub origin: Option<SceneOrigin>,
    pub events: Vec<SceneReplayEvent>,
}

/// One accepted interpreter input, in the order it occurred. Battle events represent acknowledged
/// victories only; defeat and escape are never substituted for a successful script continuation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum SceneReplayEvent {
    Window {
        step: u32,
        identity: Vec<u8>,
        #[serde(deserialize_with = "crate::required_option")]
        answer: Option<u8>,
    },
    Battle { ordinal: u32, identity: Vec<u8>, formation: u16, progress: Vec<u8>, gil: u32 },
}

impl SceneReplay {
    /// Check wire bounds and structural ordering. The game host must additionally check the exact
    /// script windows, choice ranges, battle identities and resource transitions.
    pub fn validate(&self) -> Result<(), &'static str> {
        if let Some(origin) = &self.origin { origin.validate()?; }
        if self.events.len() > MAX_SCENE_REPLAY_EVENTS { return Err("too many scene replay events"); }
        let mut window = 0u32;
        let mut battle = 1u32;
        for event in &self.events {
            let identity = match event {
                SceneReplayEvent::Window { step, identity, .. } => {
                    if *step != window || *step >= MAX_SCENE_STEPS { return Err("scene replay windows are not consecutive"); }
                    window += 1;
                    identity
                }
                SceneReplayEvent::Battle { ordinal, identity, progress, .. } => {
                    if *ordinal != battle { return Err("scene replay battles are not consecutive"); }
                    if progress.len() > crate::MAX_STORY_PROGRESS_BYTES { return Err("scene replay story exceeds its bounds"); }
                    battle += 1;
                    identity
                }
            };
            if identity.is_empty() || identity.len() > MAX_SCENE_IDENTITY_BYTES { return Err("scene replay identity exceeds its bounds"); }
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for SceneReplay {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            source: [u8; 32],
            #[serde(deserialize_with = "crate::required_option")]
            origin: Option<SceneOrigin>,
            events: Vec<SceneReplayEvent>,
        }
        let wire = Wire::deserialize(deserializer)?;
        let replay = Self { source: wire.source, origin: wire.origin, events: wire.events };
        replay.validate().map_err(serde::de::Error::custom)?;
        Ok(replay)
    }
}

/// Serde normally treats a missing Option field as None. A v20 SceneStart must explicitly say
/// whether it is a continuation; the custom field deserializer prevents the missing-field default.
pub(crate) fn required_replay<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<SceneReplay>, D::Error> {
    Option::<SceneReplay>::deserialize(deserializer)
}

pub(crate) fn replay_progress<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
    let progress = Vec::<u8>::deserialize(deserializer)?;
    if progress.len() > crate::MAX_STORY_PROGRESS_BYTES { return Err(serde::de::Error::custom("scene resume story exceeds its bounds")); }
    Ok(progress)
}

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
                gil: 1,
                replay: None,
                owner: EntityId(3),
                participants: vec![SceneParticipant { entity: EntityId(3), spot: 0 }, SceneParticipant { entity: EntityId(4), spot: 1 }],
                you: SceneRole::Watcher,
            },
            concat!(
                r#"{"SceneStart":{"run":7,"scene":"ff7/mds7pb_1/tifa-bar","seed":41,"progress":[1,72,0],"gil":1,"replay":null,"owner":3,"#,
                r#""participants":[{"entity":3,"spot":0},{"entity":4,"spot":1}],"you":"Watcher"}}"#
            ),
        );
        golden(ServerMsg::SceneStep { run: SceneRunId(7), step: 3, answer: Some(2) }, r#"{"SceneStep":{"run":7,"step":3,"answer":2}}"#);
        golden(ServerMsg::SceneEnded { run: SceneRunId(7), outcome: SceneOutcome::TimedOut }, r#"{"SceneEnded":{"run":7,"outcome":"TimedOut"}}"#);
        golden(ServerMsg::SceneRefused { reason: SceneRefusal::Unavailable }, r#"{"SceneRefused":{"reason":"Unavailable"}}"#);
    }

    #[test]
    fn scenes_require_the_new_protocol() {
        assert_eq!(crate::PROTOCOL_VERSION, 20);
    }

    #[test]
    fn continuation_messages_distinguish_a_held_battle_from_releasing_the_scene() {
        use crate::BattleId;
        golden(ClientMsg::SceneBattleReady { run: SceneRunId(7), ordinal: 1, step: 3 },
            r#"{"SceneBattleReady":{"run":7,"ordinal":1,"step":3}}"#);
        golden(ClientMsg::SceneRetry { run: SceneRunId(7) }, r#"{"SceneRetry":{"run":7}}"#);
        golden(ServerMsg::SceneBattleStarted { run: SceneRunId(7), ordinal: 1, battle_id: BattleId(9) },
            r#"{"SceneBattleStarted":{"run":7,"ordinal":1,"battle_id":9}}"#);
        golden(ServerMsg::SceneBattleResumed { run: SceneRunId(7), ordinal: 1, battle_id: BattleId(9), progress: vec![3, 39, 0], gil: 702 },
            r#"{"SceneBattleResumed":{"run":7,"ordinal":1,"battle_id":9,"progress":[3,39,0],"gil":702}}"#);
        golden(ServerMsg::SceneGameOver { run: SceneRunId(7), ordinal: 1, battle_id: BattleId(9) },
            r#"{"SceneGameOver":{"run":7,"ordinal":1,"battle_id":9}}"#);
        assert!(serde_json::from_str::<SceneOutcome>(r#""GameOver""#).is_err(), "game over holds the scene; it never means SceneEnded");
    }

    fn continuation_start(replay: Option<SceneReplay>) -> ServerMsg {
        ServerMsg::SceneStart {
            run: SceneRunId(7), scene: "ff7/md8brdg/confrontation".into(), seed: 41,
            progress: vec![3, 39, 0], gil: 600, replay, owner: EntityId(3),
            participants: vec![SceneParticipant { entity: EntityId(3), spot: 0 }], you: SceneRole::Owner,
        }
    }

    #[test]
    fn a_v20_start_requires_an_explicit_replay_even_for_ordinary_scenes() {
        for replay in [None, Some(SceneReplay { source: [0; 32], origin: None, events: vec![] })] {
            let message = continuation_start(replay);
            let bytes = crate::encode(&message).unwrap();
            assert_eq!(crate::decode::<ServerMsg>(&bytes[4..]).unwrap(), message);
            let mut old = serde_json::to_value(&message).unwrap();
            old["SceneStart"].as_object_mut().unwrap().remove("replay");
            let error = crate::decode::<ServerMsg>(&serde_json::to_vec(&old).unwrap()).unwrap_err();
            assert!(error.to_string().contains("missing field `replay`"), "{error}");
        }
        let hello_v19 = br#"{"Hello":{"protocol_version":19,"client_build":"c","auth":{"Token":"x"},"character":{"tint":[0,0,0]}}}"#;
        assert_eq!(crate::probe_hello_version(hello_v19), Some(19));
        assert_ne!(crate::probe_hello_version(hello_v19), Some(crate::PROTOCOL_VERSION));
    }

    #[test]
    fn a_replayed_line_scene_preserves_its_finite_activation_pose() {
        let origin = SceneOrigin { pos: Pos::new(-400.5, 835.0, 518.0), facing: 1.5, triangle: Some(20) };
        golden(origin, r#"{"pos":{"x":-400.5,"y":835.0,"z":518.0},"facing":1.5,"triangle":20}"#);
        for origin in [None, Some(origin), Some(SceneOrigin { triangle: None, ..origin })] {
            let replay = SceneReplay { source: [2; 32], origin, events: vec![] };
            let message = continuation_start(Some(replay.clone()));
            let frame = crate::encode(&message).unwrap();
            assert_eq!(crate::decode::<ServerMsg>(&frame[4..]).unwrap(), message);
            let mut missing = serde_json::to_value(&replay).unwrap();
            missing.as_object_mut().unwrap().remove("origin");
            assert!(serde_json::from_value::<SceneReplay>(missing).is_err(), "a missing origin must not silently choose a fixed scene pose");
        }
        for coordinate in ["x", "y", "z"] {
            let mut bad = serde_json::to_value(origin).unwrap();
            bad["pos"][coordinate] = serde_json::json!(1e40);
            assert!(serde_json::from_value::<SceneOrigin>(bad).is_err(), "non-finite {coordinate}");
        }
        let mut bad = serde_json::to_value(origin).unwrap();
        bad["facing"] = serde_json::json!(1e40);
        assert!(serde_json::from_value::<SceneOrigin>(bad).is_err());
        let mut missing = serde_json::to_value(origin).unwrap();
        missing.as_object_mut().unwrap().remove("triangle");
        assert!(serde_json::from_value::<SceneOrigin>(missing).is_err());
        for bad in [SceneOrigin { pos: Pos::new(f32::NAN, 0.0, 0.0), ..origin }, SceneOrigin { facing: f32::INFINITY, ..origin }] {
            assert!(SceneReplay { source: [2; 32], origin: Some(bad), events: vec![] }.validate().is_err());
        }
    }

    fn window(step: u32) -> SceneReplayEvent {
        SceneReplayEvent::Window { step, identity: vec![1, 9], answer: None }
    }

    fn battle(ordinal: u32) -> SceneReplayEvent {
        SceneReplayEvent::Battle { ordinal, identity: vec![4, 6], formation: 328, progress: vec![3, 39, 0], gil: 702 }
    }

    #[test]
    fn replay_preserves_every_window_and_durable_battle_resource_snapshot_in_order() {
        golden(window(0), r#"{"Window":{"step":0,"identity":[1,9],"answer":null}}"#);
        golden(SceneReplayEvent::Window { step: 1, identity: vec![2], answer: Some(3) },
            r#"{"Window":{"step":1,"identity":[2],"answer":3}}"#);
        golden(battle(1), r#"{"Battle":{"ordinal":1,"identity":[4,6],"formation":328,"progress":[3,39,0],"gil":702}}"#);
        let events = vec![window(0), battle(1), window(1), window(2), battle(2), window(3)];
        let replay = SceneReplay { source: [0xfe; 32], origin: None, events };
        assert_eq!(replay.validate(), Ok(()));
        let message = continuation_start(Some(replay));
        let bytes = crate::encode(&message).unwrap();
        assert_eq!(crate::decode::<ServerMsg>(&bytes[4..]).unwrap(), message);
        let mut damaged = serde_json::to_value(window(0)).unwrap();
        damaged["Window"].as_object_mut().unwrap().remove("answer");
        assert!(serde_json::from_value::<SceneReplayEvent>(damaged).is_err(), "even a plain window has an explicit acknowledgement");
    }

    #[test]
    fn replay_decoding_rejects_damaged_order_identities_and_resource_bounds() {
        let mut cases = vec![
            vec![window(1)], vec![window(0), window(0)], vec![window(0), window(2)],
            vec![battle(0)], vec![battle(2)], vec![battle(1), battle(1)], vec![battle(1), battle(3)],
            (0..=MAX_SCENE_REPLAY_EVENTS as u32).map(window).collect(),
        ];
        for identity in [vec![], vec![0; MAX_SCENE_IDENTITY_BYTES + 1]] {
            cases.push(vec![SceneReplayEvent::Window { step: 0, identity: identity.clone(), answer: None }]);
            cases.push(vec![SceneReplayEvent::Battle { ordinal: 1, identity, formation: 328, progress: vec![], gil: 0 }]);
        }
        cases.push(vec![SceneReplayEvent::Battle { ordinal: 1, identity: vec![1], formation: 328,
            progress: vec![0; crate::MAX_STORY_PROGRESS_BYTES + 1], gil: 0 }]);
        for events in cases {
            let replay = SceneReplay { source: [1; 32], origin: None, events };
            assert!(replay.validate().is_err(), "{replay:?}");
            let encoded = serde_json::to_vec(&continuation_start(Some(replay))).unwrap();
            assert!(crate::decode::<ServerMsg>(&encoded).is_err(), "malformed replay reached a client");
        }
        // The exact bounds remain usable, including a zero fingerprint (the host verifies its meaning).
        let replay = SceneReplay { source: [0; 32], origin: None, events: (0..MAX_SCENE_REPLAY_EVENTS as u32)
            .map(|step| SceneReplayEvent::Window { step, identity: vec![0; MAX_SCENE_IDENTITY_BYTES], answer: None }).collect() };
        assert!(serde_json::from_slice::<SceneReplay>(&serde_json::to_vec(&replay).unwrap()).is_ok());
        let resumed = ServerMsg::SceneBattleResumed { run: SceneRunId(7), ordinal: 1, battle_id: crate::BattleId(9),
            progress: vec![0; crate::MAX_STORY_PROGRESS_BYTES + 1], gil: 702 };
        assert!(crate::decode::<ServerMsg>(&serde_json::to_vec(&resumed).unwrap()).is_err());
    }

    #[test]
    fn a_scene_start_requires_the_servers_gil_snapshot_and_refuses_out_of_range_values() {
        let old = r#"{"SceneStart":{"run":7,"scene":"ff7/md8_2/station","seed":41,"progress":[1,36,0],"owner":3,"participants":[{"entity":3,"spot":0}],"you":"Owner"}}"#;
        let err = crate::decode::<ServerMsg>(old.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("missing field `gil`"), "{err}");
        for invalid in ["null", "-1", "4294967296", "1.5", "\"1\""] {
            let bad = old.replace("\"owner\":3", &format!("\"gil\":{invalid},\"owner\":3"));
            assert!(crate::decode::<ServerMsg>(bad.as_bytes()).is_err(), "accepted gil {invalid}");
        }
        let hello_v18 = br#"{"Hello":{"protocol_version":18,"client_build":"c","auth":{"Token":"x"},"character":{"tint":[0,0,0]}}}"#;
        assert_eq!(crate::probe_hello_version(hello_v18), Some(18));
        assert_ne!(crate::probe_hello_version(hello_v18), Some(crate::PROTOCOL_VERSION));
    }

    #[test]
    fn a_scene_start_round_trips_its_exact_starting_gil_for_owner_and_watchers() {
        for gil in [0, 1, u32::MAX] {
            for you in [SceneRole::Owner, SceneRole::Watcher] {
                let msg = ServerMsg::SceneStart {
                    run: SceneRunId(7), scene: "ff7/md8_2/station".into(), seed: 41,
                    progress: vec![1, 36, 0], gil, replay: None, owner: EntityId(3),
                    participants: vec![SceneParticipant { entity: EntityId(3), spot: 0 }, SceneParticipant { entity: EntityId(4), spot: 1 }],
                    you,
                };
                let encoded = crate::encode(&msg).unwrap();
                assert_eq!(u32::from_le_bytes(encoded[..4].try_into().unwrap()) as usize, encoded.len() - 4);
                assert_eq!(crate::decode::<ServerMsg>(&encoded[4..]).unwrap(), msg);
                let json: serde_json::Value = serde_json::from_slice(&encoded[4..]).unwrap();
                assert_eq!(json["SceneStart"]["gil"], gil, "even zero must be present on the wire");
            }
        }
    }

    #[test]
    fn scene_npc_ids_never_meet_an_areas_own() {
        assert!(u32::try_from(crate::MAX_NPCS_PER_AREA).unwrap() < SCENE_NPC_ID_BASE);
    }
}
