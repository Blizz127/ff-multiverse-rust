//! Story scenes on the client: the game's own scripted conversations, played from this machine's own copy of the game, with
//! the player's character standing in them as a witness.
//!
//! **Verified on owned FF7 Steam Edition (en) on 2026-11-09; other editions are unverified.**
//!
//! A player's OK next to a *scene trigger* (a character the server adds to the area's cast while a scene is on offer:
//! `fmv_protocol::SCENE_NPC_ID_BASE`) starts a scene for that player ([`trigger`]). The server answers every participant with
//! `SceneStart`; each client then plays the scene LOCALLY, from its own field: nothing but ids, a seed, the story and the
//! owner's choices crosses the wire (see `fmv_protocol::scene`). The canon cast speak their real lines in the order the
//! game's own field script runs them; the player's own character stands in the scene as a witness, on the spot the scene
//! names, and never takes a line (Cloud's lines stay Cloud's).
//!
//! The pieces, pure and testable without a GPU or game data except where said:
//!
//! * [`source`]: the seam a scene is read through ([`source::SceneSource`]; `fmv_ff7`'s `ScenePlay` is the real one) and the
//!   builder that makes one from this machine's own field ([`source::SceneBuilder`], [`Ff7Scenes`]).
//! * [`stage`]: the cast's places, turns and visibility and the camera's record, moved on by the scene's clock.
//! * [`panel`]: one window of the scene (the talk window's layout and look), placed near its speaker.
//! * [`player`]: the state machine of one run, in both roles (owner and watcher).
//! * [`client`]: what a client holds ([`SceneClient`], in `ClientState`): the server's messages in, the player's keys and
//!   time, what is owed to the server and what happened out. Control is never left locked.
//! * [`keys`]: where a key goes while a scene may be on show (the scene's, F6's preview, the OK that starts one, or the
//!   field's).
//! * [`draw`]: what a frame of the field shows while a scene runs (the cast, the witnesses, the hidden trigger, the
//!   window's anchor).
//! * [`trigger`]: the OK that starts a scene.
//! * [`bot`]: the headless bot (`--scene-preview`, `--scene-watch`, ...), with the output lines a smoke test reads.
//!
//! # Honest limits
//!
//! * Matching field-model cast can play normal-speed full-range default loops on scene time. Canonical appearance
//!   substitutions and unsupported animation forms keep their usual clips; skipped events are counted ([`stage::Counts`]).
//!   A standing loop yields to movement; one-shot poses, cropped ranges and dynamic rates remain unsupported.
//! * The scene's camera moves are recorded ([`stage::CameraState`]), not shown: the field's camera stays where it is.
//! * A window pauses the whole stage (as the interpreter pauses the whole scene); the game pauses only the script that
//!   opened it.
//! * Durations (a walk's length) are the interpreter's estimates: the picture is paced by them.
//! * A private bar scene may use Echo-S after its choice paths are checked against the original. A scene with watchers
//!   stays on the original because Echo-S has different window counts; other scenes are not yet Echo-S enabled.

mod bot;
mod client;
mod draw;
mod keys;
mod panel;
mod player;
mod source;
mod stage;
mod trigger;

#[cfg(test)]
mod shots;
#[cfg(test)]
pub(crate) mod tests;

pub use bot::{pick_probe, Act, SceneBot, SceneBotConfig, Senses};
pub use client::{clean_id, SceneClient};
pub use draw::{cast_players, label_hidden, place_witnesses, speaker_spot, visible_npcs};
pub use keys::{route_key, SceneRoute};
pub use player::{SceneCmd, SceneSound, SoundAction};
pub use source::Ff7Scenes;
#[cfg(test)]
pub(crate) use source::{Built, SceneBuilder};
