//! fmv-ff7 — FF7 (PC) adapter: format decoders and the FF7 area runtime.
//!
//! **Verified on owned FF7 Steam Edition (en) on 2026-11-08; every other edition is
//! unverified.** The format layouts come from public community documentation (URLs are cited in
//! each module). They were written without game data and then checked against the owner's
//! `flevel.lgp` (753 entries, 702 of them fields) and `char.lgp` (12 649 entries): the LGP
//! reader, the LZSS decoder, the field container, walkmesh, camera, triggers/gateways, palette
//! and background decoders all agree with the real files, and each module lists the measured
//! facts it rests on (counts and sizes only, never game bytes). What the checks found and fixed:
//! a palette section with zero pages is valid (14 direct-colour fields), palette index 0 is
//! transparent whatever its colour, `control_direction` is unsigned, and the background section
//! ends with an `END` tag; for the character models the checks found that 105 skeletons declare
//! `:BONES 0` and list one bone, that normal indices are absolute, and that the posing conventions
//! the documentation leaves out are the standard matrices of a y-down right-handed frame.
//! Unit tests use small synthetic data built in test code; the `#[ignore]`d tests in
//! `tests/real_data.rs` and the `field_dump` and `model_dump` examples (all read the data directory
//! from `FMV_FF7_DATA`) check the real files.
//!
//! Modules:
//!
//! * [`lgp`]   - LGP archive reader over any `Read + Seek` (a library blob path works).
//! * [`lzss`]  - FF7 LZSS decompressor (plus a test-only compressor).
//! * [`field`] - decompressed PC field file: sections, walkmesh, camera, palette,
//!   triggers/gateways, background tiles and per-tile pixel decode.
//! * [`background`] - the renderable background: [`background::decode_background`] composites
//!   the layers into one RGBA image and [`background::Projection`] maps walkmesh coordinates
//!   onto it through the field camera.
//! * [`area`]  - [`Ff7Adapter`] / [`Ff7FieldArea`] implementing the `fmv-adapter-api`
//!   traits over a `flevel.lgp` source. Synthetic provenance is labelled FIXTURE; the area
//!   fingerprint binds the edition provenance and is defined by [`area_fingerprint`].
//!   Movement is validated against the walkmesh *and* its edge-access table (internal
//!   `walk` module; see the `area` docs for the rule). A loaded field also reports its gateway
//!   exits, its non-player characters, an arrival placement and its random-encounter table
//!   (`AreaRuntime::{exits, npcs, place_arrival, encounters}`, see the `area` docs).
//! * [`decode_encounter_section`] - the random-encounter table of a field (section 6).
//! * [`field_direction_to_facing`] - the script's `DIR` byte as a protocol heading (the one
//!   conversion, with the evidence for it).
//!
//! Field character models (from `field/char.lgp`, verified on the same owned edition):
//!
//! * [`model`] - [`load_field_model`] reads a character into render-ready [`FieldModel`] data
//!   (bones, [`MeshPart`]s with positions, normals, UVs, vertex colours, indices, textures);
//!   [`model_to_field`] places it in a field; the coordinate conventions are documented there.
//! * [`anim`]  - [`FieldAnimation`]: per-frame bone rotations and root motion, and
//!   [`FieldAnimation::pose`] giving per-bone matrices. The posing conventions (which the
//!   documentation does not state) and how they were established are in its docs.
//! * [`hrc`], [`rsd`], [`pmodel`], [`tex`] - the skeleton, resource, mesh and texture formats.
//! * [`playable`] - the stable appearance ids (`ff7/cloud`, ...) and which skeleton and
//!   animations each means ([`resolve_appearance`]).
//!
//! Battle models (from `battle/battle.lgp`, verified on the same owned edition):
//!
//! * [`battle_model`] - enemies, battle locations and the playable characters' battle models,
//!   with their bit-packed animations, as the same [`FieldModel`] and [`FieldAnimation`] types the
//!   field characters use (`BattleModels::load_enemy` / `load_location` / `load_character`).
//!
//! # Wiring notes for the integrator
//!
//! * Call [`Ff7Adapter::prewarm`] on a blocking thread (for example `spawn_blocking`) before
//!   networking starts: the first `list_areas()` reads the whole archive.
//! * Fold [`adapter_version`] into the client/server build strings so two builds with
//!   different fingerprint rules ([`FINGERPRINT_RULE_VERSION`]) are not mistaken for two
//!   players with different media.
//! * Build the provenance honestly: `Platform::Synthetic` (and `SourceRef::Fixture`) for
//!   synthetic FF7-format data, which makes every area FIXTURE.
//!
//! Hard rules this crate follows: originals are opened read-only and never modified; no
//! game asset bytes are logged, serialised or sent anywhere; untrusted input (archives,
//! compressed streams, field files) is parsed without panicking and with every allocation
//! bounded.

#![forbid(unsafe_code)]
// Untrusted-data hygiene, enforced by `cargo clippy -p fmv-ff7`: no unwrap/expect/panic or
// unchecked indexing in non-test code.
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        clippy::todo,
        clippy::unimplemented
    )
)]

// Test code reads better with `chunks_exact(n)` and `% 2 == 0` than with `as_chunks` and
// `is_multiple_of`, which need newer compilers than the workspace's MSRV (see `lgp`).
#![cfg_attr(test, allow(clippy::chunks_exact_to_as_chunks, clippy::manual_is_multiple_of))]

pub mod anim;
pub mod area;
pub mod background;
pub mod battle_model;
pub mod field;
pub mod hrc;
pub mod lgp;
pub mod lzss;
pub mod model;
pub mod playable;
pub mod pmodel;
pub mod rsd;
pub mod script;
pub mod story;
pub mod tex;

mod area_content;
mod encounter;
mod util;
mod walk;

#[cfg(test)]
mod area_hooks_tests;
#[cfg(test)]
mod testutil;

pub use area_content::{field_direction_to_facing, MAX_NPCS_PER_AREA, MAX_NPC_LABEL_CHARS, MAX_NPC_SECTION_BYTES};
pub use encounter::decode_encounter_section;

pub use area::{
    adapter_version, area_fingerprint, Ff7Adapter, Ff7FieldArea, SkippedEntry, FF7_FIELD_FRAMES_PER_SECOND,
    FF7_FIELD_MAX_SPEED_PLACEHOLDER, FF7_FIELD_RUN_SPEED_BASE, FF7_FIELD_RUN_UNITS_PER_FRAME,
    FF7_FIELD_WALK_SPEED_BASE, FF7_FIELD_WALK_UNITS_PER_FRAME, FF7_RUN_WALK_SPEED_RATIO, FINGERPRINT_RULE_VERSION,
    MAX_FIELD_BYTES, MAX_INDEX_READ_BYTES,
};
pub use walk::LinkReport;

pub use anim::FieldAnimation;
pub use model::{
    default_model_scale, load_field_animation, load_field_model, model_to_field, FieldModel, MeshPart, ModelBone, ModelError,
    PartBlend,
};
pub use playable::{load_playable, playable_field_models, resolve_appearance, Character, PlayableModel, DEFAULT_APPEARANCE};
pub use tex::RgbaImage;

/// Status string a surface that presents FF7 data from this crate can show. The decoders have
/// been confirmed against one owned edition only; any other edition (PS1, 1998 PC, re-releases
/// with different layouts) still counts as unverified.
pub const VERIFICATION_STATUS: &str = "VERIFIED ON OWNED FF7 STEAM EDITION (EN) 2026-11-08; OTHER EDITIONS UNVERIFIED";
