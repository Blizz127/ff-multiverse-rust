//! Players drawn as the models their binding names.
//!
//! In window mode on an FF7 field, every player (the local one included) is drawn as the model its
//! entity's `appearance_ref` names: its BINDING (protocol v8), how the game draws the character, which is a
//! different thing from the combat slot it fights in (a player who holds Barret's slot is not drawn as Barret).
//! For a new character the binding is one of the generic placeholder bodies of the owner's own `char.lgp`
//! (`ff7/body/<archetype>`, [`fmv_ff7::playable::PLACEHOLDER_BODIES`]), which is **repainted in the player's
//! identity colour** ([`identity`]: a per-draw [`identity::Identity`] the fragment shader turns the cloth into, in
//! the colour's hue and saturation at the cloth's own lightness, while skin and the face decals keep their own
//! colours, so the colour is plainly the player's on dark and saturated cloth and faces stay readable); for an
//! account that predates v8 it is the canon model of the look it chose
//! (`ff7/tifa`), which keeps its own colours like every canon model (the colour is the ring under its feet). The
//! ref is looked up in both tables ([`fmv_ff7::playable::resolve_model`]); an unknown or missing one stands in as
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
//! ([`library::MAX_NPC_MODELS`]: what is left of the GPU's 32 models once the players have their 8 canon characters and
//! up to 4 placeholder bodies) because a server can name new ones in every area.
//!
//! The pieces:
//!
//! * [`source`]: where `char.lgp` is and the loader over it (the [`source::CharacterLoader`] seam).
//! * [`library`]: each appearance loaded once, merged ([`mesh`]) and cached by the table's own id;
//!   the non-player models by the stems the area names.
//! * [`anim`]: which clip an avatar plays (idle, walk, run) and its phase in it.
//! * [`stage`]: the field facts models need (camera, model scale, speeds, the depth-tested scenery).
//! * [`frame`]: pose, place and skin the visible players into camera space, plus the ground marks
//!   and the name-label anchors: a pure CPU step, unit-tested without a GPU.
//! * [`ground`]: the shadow and rings under the players.
//! * [`scenery`]: the field's picture as depth-tested tiles (atlas, quads, blend runs, the depth
//!   mapping every depth test of a frame shares), built once per field.
//! * [`identity`]: the player's colour on a model: the per-draw [`identity::Identity`], the shader's look and the
//!   CPU mirror the tests hold it to.
//! * [`gpu`]: upload once per character, depth-tested drawing through the field camera, the marks on
//!   the ground, and the passes of a frame; [`scenery_gpu`]: the tile pipelines.
//!
//! # Layers
//!
//! The PC engine does not paste characters onto a finished picture: every background tile is a quad
//! with a depth, in the same z-buffered pass as the models, and a character is hidden by exactly the
//! tiles that are nearer to the camera than it is. A layer-2 tile's depth is `4 * id` in camera-space
//! units (the unit of the camera depth the models are skinned to), layer 4 is in front of everything,
//! layers 1 and 3 are far behind it; this is `fmv_ff7::background::depth`'s rule, derived there from
//! the engine's own stored values and the walkmesh, and verified on all 702 owned fields.
//!
//! In window mode on a field with something in front of the characters (any tile of layer 2 or 4) the
//! picture is therefore drawn **tile by tile** ([`scenery`]), in this order:
//!
//! 1. the picture's opaque tiles, depth written, over a black base (pixels no tile touches are black,
//!    as in the game);
//! 2. the walkmesh overlay (F1) and the markers of players without a model;
//! 3. the marks on the ground, depth tested at the depth of the feet (so they are hidden when the
//!    character is, and not when only something farther overlaps them);
//! 4. the models, depth tested against the tiles;
//! 5. the picture's blended tiles (glows, shadows, glass; any of the four PlayStation modes), over
//!    the models, depth tested but not written, in paint order.
//!
//! Every depth of the frame goes through one mapping fixed for the field: the PC engine's
//! perspective z calibrated from its baked depths, or the existing near-50/far-32 000 fallback.
//! Reliable calibrations also enable homogeneous model clipping; fields without one retain the
//! old clamped geometry policy. See [`scenery`] for why fitting to the models would be wrong. At one window
//! pixel per picture pixel the tile picture is the flat picture bit for bit (a test checks it on
//! `md1stin` and `md1_1`); a field with nothing in front, or a frame without models, still draws the
//! flat picture as before.
//!
//! # Known limits
//!
//! * **The depth rule is verified; its inputs are approximate.** Measured on the owned data
//!   (`md1stin`, offscreen, Cloud facing the camera; the numbers of `shots/depth.rs`, agreeing with
//!   the software composites of `fmv-ff7`'s `examples/depth_composite.rs` to within two pixels at each
//!   spot and to 0.2% over all of them): at the place where the old flat front layer hid 1 296 of Cloud's 3 304
//!   pixels (39%) behind the far left wall piece (layer 2, id 544) although he is nearer than it, he
//!   is now fully visible; behind that wall piece he is hidden (149 of 149 pixels on walkmesh triangle
//!   38) or partly hidden (63 of 145 on triangle 36); behind the posts (ids 152 and 294) 27% and 25%;
//!   behind the train 85%. Over the 73 walkable places where a character is a good size (camera depth
//!   450 to 1 300), where the flat layer hid 44 784 of his 67 450 pixels, the depth rule hides 15 605
//!   (35%). This replaces the earlier note that the id could not be interpreted (the flat layer's
//!   overlap with a character's outline box at 59 of the 73 places was the flat layer's reach, not
//!   what the game hides).
//!   What is still approximate: depth is **per tile**, a plane at `4 * id`, so an object that extends
//!   in depth under one id (a long wall, the train) hides a character as if it stood at one depth (the
//!   floor at the foot of the layer-2 objects measured 0.89 to 1.20 times `4 * id`, median 1.01, 83.6%
//!   of 4 015 objects within 0.8 to 1.25); and the engine's own tile depth carries the rounding of its
//!   fixed-point camera, a few units (the stored value of `md1stin` fits `4 * id - 4.6`), which is
//!   not applied.
//! * Fields with several camera records are drawn through camera 0, like the rest of the crate; the
//!   animation groups are the static ones (script state is not evaluated); the picture's blended
//!   tiles are drawn after all the opaque tiles and the models, so a blended tile that shares its id
//!   with an opaque tile that the file paints after it blends over it (the flat picture would
//!   not); on an sRGB target the blend unit blends in linear space, not in the game's byte
//!   arithmetic (the window prefers a non-sRGB surface, where they agree to rounding; the glows are
//!   added after the filter instead of before it, so a few saturated lamp cores are brighter than in
//!   the flat picture).
//! * Tiles are filtered (the picture is shown at any window size) and cut at half alpha, so a silhouette
//!   is hard-edged rather than blended with what is behind it; the texels a nearer tile covers
//!   carry its colour so that no key-colour green runs round an object (`md1stin`'s back layer
//!   holds 6 277 such pixels). At other window sizes the tile picture differs from the flat one by
//!   more than 24 levels at 0.03 to 0.04% of the pixels of `md1stin` (1100x680 and 1920x1080), along
//!   silhouettes.
//! * The marks on the ground (shadow, rings) are tested at one depth, the feet's, for the whole mark
//!   (about 30 units across); the markers of players and non-player characters that have no model
//!   are drawn over the scenery without a depth test (they are a fallback, not a picture); and the
//!   name label of a character that is hidden behind the scenery is still drawn over it (labels are
//!   the UI overlay, placed over the head's position, with no notion of what hides it).
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
pub mod identity;
pub mod library;
pub mod mesh;
pub mod scenery;
pub mod scenery_gpu;
pub mod source;
pub mod stage;

#[cfg(test)]
mod shots;

pub use anim::Who;
pub use frame::{AvatarFrame, AvatarPlayer, Avatars};
pub use stage::Stage;
