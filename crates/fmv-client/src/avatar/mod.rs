//! Players drawn as their FF7 field character models.
//!
//! In window mode on an FF7 field, every player (the local one included) is drawn as the model of
//! the character they chose: the avatar's `appearance_ref` (`ff7/tifa`) is looked up in the table of
//! playable characters ([`fmv_ff7::resolve_appearance`]); an unknown or missing one stands in as
//! Cloud with the player's tint shown as a ring under the feet, so such players can still be told
//! apart. The models are read from **this machine's own** `char.lgp` (found through
//! `fmv_library::ff7::resolve_char_lgp`, from the same library set as the field archive) and never
//! come from the network: all the wire carries is the appearance id. Without that archive, or for a
//! character that fails to load, the player keeps the plain marker, exactly as before; FIXTURE areas
//! always do.
//!
//! The area's non-player characters go through the same pipeline: each is an [`AvatarPlayer`] with
//! [`frame::NpcRef`] (the skeleton and the standing clip its data names, see `crate::npc`), drawn
//! standing in that clip with a shadow under it and no rings, and a marker when its model is not
//! to be had (never the default character). The players come before them when the number of
//! models drawn is capped, and the models are cached under the names the area gave, bounded
//! ([`library::MAX_NPC_MODELS`]) because a server can name new ones in every area.
//!
//! The pieces:
//!
//! * [`source`]: where `char.lgp` is and the loader over it (the [`source::CharacterLoader`] seam).
//! * [`library`]: each appearance loaded once, merged ([`mesh`]) and cached by the table's own id;
//!   the non-player models by the stems the area names.
//! * [`anim`]: which clip an avatar plays (idle, walk, run) and its phase in it.
//! * [`stage`]: the field facts models need (camera, model scale, speeds, the scenery in front).
//! * [`frame`]: pose, place and skin the visible players into camera space, plus the ground marks
//!   and the name-label anchors: a pure CPU step, unit-tested without a GPU.
//! * [`ground`]: the shadow and rings under the players.
//! * [`gpu`]: upload once per character, depth-tested drawing through the field camera, and the
//!   scenery in front of the models.
//!
//! # Layers
//!
//! The picture is drawn first as one image (all background layers, as before), then the models
//! with a depth buffer, then the *front* half of the scenery (layers 2 and 4) over them with alpha,
//! so a character walking behind a pillar is hidden by it. The back half is never shown on its own
//! (it is not a finished picture: it holds key-colour pixels that the front half covers). When the
//! front layers hold blended tiles, which a flat image cannot reproduce, the models are drawn over
//! the whole picture and nothing goes over them (see [`stage::Stage::front`]).
//!
//! # Known limits
//!
//! * **The front half is one flat image.** The game depth-sorts layer-2 tiles against each
//!   character through the tile `id`; this does not interpret it (`fmv_ff7` documents the mapping
//!   as unverified, and measured on the owned data it is not a simple function of camera depth:
//!   in `md1_1` the ids rise as objects get nearer, in `mds7st1` as they get farther, and
//!   `md1stin`'s layer 2 has only six distinct ids). So wherever the front layer is opaque it is
//!   drawn over a character, even one the game draws in front of that object. How far this
//!   reaches in `md1stin` (`offscreen_real_ff7_avatars_in_the_field` prints it): of 73 sampled
//!   walkable places where a character is a good size (camera depth 450 to 1300), the layer
//!   overlaps the character's bounding box at 59 and leaves 14 clear; the objects behind that are
//!   (layer, id) `(2, 544)` (the far left wall piece) at 23 places, `(2, 152)` and `(2, 294)` (posts
//!   near the track) at 14 each, `(2, 1)` (the train) at 7, `(2, 4088)` at 1. The train and the
//!   posts plausibly are in front of much of the walkable area; the left wall piece most likely
//!   is not (the floor under its lower edge is 1 404 units from the camera, as far as the
//!   deepest walkable place), so it probably hides the heads of characters standing before it
//!   (`54-avatars-front-layer-over-character.png` shows such a case). The next step is a depth per object
//!   (calibrated against the real engine, which this machine cannot run), drawn as quads in the
//!   model pass so the depth buffer decides.
//! * Model size and the walk/run speeds are the `fmv_ff7` measurements (see its docs): hypotheses
//!   that agree with each other and with the art, not read from the game.
//! * Models are lit by their vertex colours only, as `fmv_ff7` documents; the field's own light
//!   entries are not applied. Edges are not anti-aliased.

// `chunks_exact(4)` over RGBA bytes would be `as_chunks` on Rust 1.88+; the workspace keeps its MSRV
// lower (see `fmv-ff7`'s `lgp` module), so the lint is allowed here as it is there.
#![allow(clippy::chunks_exact_to_as_chunks)]

pub mod anim;
pub mod frame;
pub mod gpu;
pub mod ground;
pub mod library;
pub mod mesh;
pub mod source;
pub mod stage;

#[cfg(test)]
mod shots;

pub use anim::Who;
pub use frame::{AvatarFrame, AvatarPlayer, Avatars};
pub use stage::Stage;
