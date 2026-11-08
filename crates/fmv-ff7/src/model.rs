//! Field character models: [`load_field_model`] reads a character from `field/char.lgp`
//! (skeleton, meshes, textures) into render-ready data, [`FieldAnimation`] poses it, and
//! [`model_to_field`] places it in a field.
//!
//! Format details live in the modules of the individual files: [`crate::hrc`] (skeleton),
//! [`crate::rsd`] (mesh and texture names), [`crate::pmodel`] (meshes), [`crate::tex`]
//! (textures), [`crate::anim`] (animations and the posing conventions) and [`crate::playable`]
//! (which character is which).
//!
//! **Verified on owned FF7 Steam Edition (en) on 2026-11-08**: every one of the 385 `.hrc` files
//! of `field/char.lgp` loads as a complete model (see the ignored test `char_models_all_parse` in
//! `tests/real_data.rs`, which asserts the totals), and `examples/model_dump.rs` renders Cloud
//! (`aaaa.hrc`) through the `md1stin` camera onto that field's background. Other editions are
//! unverified.
//!
//! # Using it
//!
//! ```no_run
//! # use fmv_ff7::lgp::LgpArchive;
//! # use fmv_ff7::model::{load_field_animation, load_field_model, mat4_mul, model_to_field, transform_point};
//! # fn demo(path: &std::path::Path) -> Result<(), fmv_ff7::model::ModelError> {
//! let mut char_lgp = LgpArchive::open_path(path)?;               // field/char.lgp, read-only
//! let cloud = load_field_model(&mut char_lgp, "aaaa")?;          // see `playable` for the ids
//! let walk = load_field_animation(&mut char_lgp, "aaff")?;
//! // once per frame: pose, then place in the field (here: standing at walkmesh (x, y, z),
//! // heading `facing` radians, as the protocol's `facing`)
//! if let Some(pose) = cloud.pose(&walk, 12) {
//!     let place = model_to_field([3800.0, 28000.0, 380.0], 0.5, 4.0);
//!     for part in &cloud.parts {
//!         let to_field = mat4_mul(&place, &pose[part.bone_index]);
//!         // draw part.indices over part.positions transformed by `to_field`,
//!         // coloured by part.colors (and part.texture where there is one)
//!         let _first_vertex = part.positions.first().map(|p| transform_point(&to_field, *p));
//!     }
//! }
//! # Ok(()) }
//! ```
//!
//! # Coordinate conventions
//!
//! * **Part vertices** ([`MeshPart::positions`]) are in the owning bone's own frame, in the units
//!   of the file (no scale applied). [`FieldAnimation::pose`] gives, per bone, the column-major
//!   matrix from that frame to the **native model space**, and [`model_to_field`] the matrix from
//!   native model space to **field space**; the product is what a renderer multiplies a part's
//!   vertices with.
//! * **Native model space** (documented and evidenced in [`crate::anim`]): right-handed, `x` is the
//!   character's *left*, `y` points *down* (the chest is at negative y) and the character faces
//!   `-z`; the root translation's height has already been applied, so the soles of a standing
//!   character are at y of about 0.75 (0.5 to 1 for the standing and walking clips) and the hip
//!   at about -13.5 for Cloud.
//! * **Field space** is the space of the walkmesh and of [`crate::background::Projection`]:
//!   right-handed, `x`/`y` the ground plane, `z` up (verified: the camera's rotation has
//!   determinant +1 and its down axis is `-z`, see [`crate::background`]). [`model_to_field`] maps
//!   the character's forward direction (`-z` natively) to the direction `(cos facing, sin
//!   facing, 0)`, its up to `+z` and its left to the left of that heading, then scales
//!   and translates. The mapping is a proper rotation (no mirroring), so triangle winding is
//!   unchanged by it.
//! * **Winding.** The files store triangles clockwise seen from outside (97.5% of the owned
//!   polygons; see [`crate::pmodel`]); the loader swaps two corners, so [`MeshPart::indices`] are
//!   **counter-clockwise seen from outside** in both native and field space (the default front
//!   face of wgpu and OpenGL).
//! * **Scale.** One model unit is [`DEFAULT_MODEL_SCALE_UNITS`] walkmesh units in a field of scale
//!   512 ([`default_model_scale`] applies the field's own scale); this factor is a measurement
//!   against the `md1stin` art, **UNVERIFIED** (see its docs).
//!
//! # Rendering notes
//!
//! * Vertex colours ([`MeshPart::colors`]) are the baked material and shading: drawing them
//!   unlit already reads as the character (the game additionally lights models with the three
//!   light entries of the field's model loader, [`crate::field::LoaderModel::lights`], which this
//!   crate decodes but does not apply).
//! * Textures are used by the face overlay polygons of the characters (eyes, brows, mouth):
//!   [`MeshPart::texture`] is `Some` for the 695 textured groups of the owned archive. Texel alpha is
//!   0 where the colour key makes the texture transparent; draw with an alpha test (alpha below 128
//!   is discarded).
//! * [`MeshPart::blend`] and [`MeshPart::alpha`] describe the rare semi-transparent groups
//!   (23 of 4 875 in the owned archive); their interpretation is UNVERIFIED, see
//!   [`crate::pmodel::PRenderState`].
//!
//! # Safety against untrusted input
//!
//! Every file is parsed by a module that bounds its allocations by the file's own size; on top of
//! that a model may read at most [`MAX_MODEL_LOAD_BYTES`] of archive entries in total, so an archive
//! whose skeleton lists thousands of large meshes cannot make a load expensive.

use crate::anim::FieldAnimation;
use crate::hrc::Hrc;
use crate::lgp::{LgpArchive, LgpError};
use crate::pmodel::{PModel, PRenderState};
use crate::rsd::Rsd;
use crate::tex::{parse_tex, RgbaImage};
use crate::util::entry_stem;
use std::collections::HashMap;
use std::io::{Read, Seek};
use std::sync::Arc;
use thiserror::Error;

/// Cumulative cap on the archive entry bytes one [`load_field_model`] call reads (64 MiB, far more
/// than any owned model needs: the largest single `char.lgp` entry is 124 236 bytes).
pub const MAX_MODEL_LOAD_BYTES: u64 = 64 * 1024 * 1024;

/// Walkmesh units per model unit for a field of scale 512. **UNVERIFIED**: no document gives the
/// relation. It is a measurement against the `md1stin` art: with 4.0, Cloud (33.2 model units from
/// sole to the tip of his hair) is 136 walkmesh units tall and, at the spawn point of `md1stin`
/// nearest the camera, 57 pixels of the 240-pixel-high field screen (24%: the proportion of the
/// characters on FF7's field screens as the author remembers them; 3.0 would give 43 pixels and 5.0
/// 72); and he stands plausibly against the platform, the train and the far doorway (see
/// `examples/model_dump.rs`, `md1stin_cloud.png`). A rough cross-check with the animation: the planted
/// foot sweeps back 9.06 model units per step in Cloud's walk (a 15-frame step, about 0.5 to 0.6 units
/// per frame) and 14.86 in his run (a 7.5-frame step, about 2 to 2.5 per frame); times 4 that is
/// about 2.0-2.4 and 8-10 walkmesh units per frame, the hypothesised walk and run speeds of
/// [`crate::FF7_FIELD_WALK_SPEED_BASE`] and [`crate::FF7_FIELD_RUN_SPEED_BASE`]. Both are
/// hypotheses, so this shows only that they agree with each other, not that either is right.
pub const DEFAULT_MODEL_SCALE_UNITS: f32 = 4.0;

/// Errors from decoding or loading character model data.
#[derive(Debug, Error)]
pub enum ModelError {
    #[error("{what}: {why}")]
    Invalid { what: &'static str, why: String },
    #[error("{what}: data ends early (needs {needed} more bytes at offset {at})")]
    Truncated { what: &'static str, at: usize, needed: usize },
    #[error("{what}: count {count} exceeds the limit of {limit}")]
    TooMany { what: &'static str, count: u64, limit: u64 },
    #[error("char.lgp has no {kind} entry named {name:?}")]
    Missing { kind: &'static str, name: String },
    #[error("char.lgp: {0}")]
    Archive(#[from] LgpError),
    #[error("the model needs more than {limit} bytes of archive entries")]
    Budget { limit: u64 },
}

/// One bone of a loaded skeleton (what [`FieldAnimation::pose`] needs).
#[derive(Debug, Clone, PartialEq)]
pub struct ModelBone {
    pub name: String,
    /// Index of the parent bone (always smaller than this bone's index) or `None` for the root.
    pub parent: Option<usize>,
    /// Bone length in model units: the child's origin is `length` away along `-z` of this bone.
    pub length: f32,
}

/// How a mesh part is blended (see [`crate::pmodel::PRenderState`]; UNVERIFIED interpretation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartBlend {
    /// Drawn solid (every part of the standard characters).
    Opaque,
    /// Source-alpha blending with the part's constant [`MeshPart::alpha`].
    Alpha,
    /// Source-alpha scaled light added to the scene.
    Additive,
}

/// One drawable piece of a model: the triangles of one `.p` group, with deduplicated vertices.
/// All arrays are indexed by vertex; `indices` index them in threes.
#[derive(Clone)]
pub struct MeshPart {
    /// The bone whose pose matrix moves this part.
    pub bone_index: usize,
    /// Positions in the bone's own frame (model units, unscaled).
    pub positions: Vec<[f32; 3]>,
    /// Unit normals in the bone's frame (zero when the file gives none for the corner).
    pub normals: Vec<[f32; 3]>,
    /// Texture coordinates, `v = 0` at the top row of [`MeshPart::texture`] (zero for untextured
    /// parts).
    pub uvs: Vec<[f32; 2]>,
    /// RGBA vertex colours (baked material and shading).
    pub colors: Vec<[u8; 4]>,
    /// Triangle list, counter-clockwise seen from outside (see the module docs).
    pub indices: Vec<u32>,
    /// The part's texture, if it has one and it could be loaded. Transparent texels have alpha 0.
    pub texture: Option<Arc<RgbaImage>>,
    /// Stem of the texture entry (`aabb` for `aabb.tex`), also when it could not be loaded.
    pub texture_name: Option<String>,
    pub blend: PartBlend,
    /// Constant alpha of a blended part (1.0 when opaque).
    pub alpha: f32,
}

/// `Debug` prints counts only: decoded game geometry must not leak into logs by accident.
impl std::fmt::Debug for MeshPart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MeshPart")
            .field("bone_index", &self.bone_index)
            .field("vertices", &self.positions.len())
            .field("triangles", &(self.indices.len() / 3))
            .field("texture", &self.texture_name)
            .field("blend", &self.blend)
            .finish()
    }
}

impl MeshPart {
    /// Number of triangles.
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }
}

/// A loaded character: skeleton plus meshes.
#[derive(Debug, Clone)]
pub struct FieldModel {
    /// Stem of the `.hrc` entry, lower case (`aaaa`).
    pub hrc_name: String,
    /// The `:SKELETON` name inside it (`n_cloud_sk`).
    pub skeleton_name: String,
    pub bones: Vec<ModelBone>,
    pub parts: Vec<MeshPart>,
    /// Triangles left out because an index was out of range (0 in the owned archive).
    pub dropped_triangles: usize,
    /// Texture entries that were named but could not be loaded (empty in the owned archive).
    pub missing_textures: Vec<String>,
}

impl FieldModel {
    /// Whether `animation` can drive this skeleton (same bone count).
    pub fn fits(&self, animation: &FieldAnimation) -> bool {
        animation.fits(&self.bones)
    }

    /// Per-bone matrices for a frame of `animation` (see [`FieldAnimation::pose`]); `None` if the
    /// animation does not fit.
    pub fn pose(&self, animation: &FieldAnimation, frame: usize) -> Option<Vec<[f32; 16]>> {
        animation.pose(&self.bones, frame)
    }

    /// Total triangles over all parts.
    pub fn triangle_count(&self) -> usize {
        self.parts.iter().map(MeshPart::triangle_count).sum()
    }

    /// Axis-aligned bounds `(min, max)` of every vertex after `transform` (a model-to-somewhere
    /// matrix) applied on top of the bone matrices in `pose`. `None` for a model without vertices
    /// or a pose with too few matrices.
    pub fn posed_bounds(&self, pose: &[[f32; 16]], transform: &[f32; 16]) -> Option<([f32; 3], [f32; 3])> {
        let mut bounds: Option<([f32; 3], [f32; 3])> = None;
        for part in &self.parts {
            let m = mat4_mul(transform, pose.get(part.bone_index)?);
            for &p in &part.positions {
                let q = transform_point(&m, p);
                bounds = Some(match bounds {
                    None => (q, q),
                    Some((lo, hi)) => (
                        [lo[0].min(q[0]), lo[1].min(q[1]), lo[2].min(q[2])],
                        [hi[0].max(q[0]), hi[1].max(q[1]), hi[2].max(q[2])],
                    ),
                });
            }
        }
        bounds
    }
}

// ---------------------------------------------------------------------------------------
// Matrices (column-major `[f32; 16]`, compatible with `glam::Mat4::from_cols_array`)
// ---------------------------------------------------------------------------------------

/// The identity matrix.
pub fn mat4_identity() -> [f32; 16] {
    [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0]
}

/// `a * b` for column-major matrices (applying the result to a point applies `b` first).
pub fn mat4_mul(a: &[f32; 16], b: &[f32; 16]) -> [f32; 16] {
    std::array::from_fn(|index| {
        let (column, row) = (index / 4, index % 4);
        (0..4)
            .map(|k| a.get(k * 4 + row).copied().unwrap_or(0.0) * b.get(column * 4 + k).copied().unwrap_or(0.0))
            .sum()
    })
}

/// Translation matrix.
pub fn mat4_translate(t: [f32; 3]) -> [f32; 16] {
    [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, t[0], t[1], t[2], 1.0]
}

/// Uniform scale matrix.
pub fn mat4_scale(s: f32) -> [f32; 16] {
    [s, 0.0, 0.0, 0.0, 0.0, s, 0.0, 0.0, 0.0, 0.0, s, 0.0, 0.0, 0.0, 0.0, 1.0]
}

/// Rotation about +z by `radians` (counter-clockwise seen from above, right-handed).
pub fn mat4_rotate_z(radians: f32) -> [f32; 16] {
    let (s, c) = radians.sin_cos();
    [c, s, 0.0, 0.0, -s, c, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0]
}

/// Apply `m` to a point (w = 1).
pub fn transform_point(m: &[f32; 16], p: [f32; 3]) -> [f32; 3] {
    let at = |i: usize| m.get(i).copied().unwrap_or(0.0);
    [
        at(0) * p[0] + at(4) * p[1] + at(8) * p[2] + at(12),
        at(1) * p[0] + at(5) * p[1] + at(9) * p[2] + at(13),
        at(2) * p[0] + at(6) * p[1] + at(10) * p[2] + at(14),
    ]
}

/// Apply the rotation/scale part of `m` to a direction (w = 0), for normals.
pub fn transform_vector(m: &[f32; 16], v: [f32; 3]) -> [f32; 3] {
    let at = |i: usize| m.get(i).copied().unwrap_or(0.0);
    [
        at(0) * v[0] + at(4) * v[1] + at(8) * v[2],
        at(1) * v[0] + at(5) * v[1] + at(9) * v[2],
        at(2) * v[0] + at(6) * v[1] + at(10) * v[2],
    ]
}

/// Map the character's native model space to field (walkmesh) space: a character standing at
/// `position` (its feet; walkmesh `x, y, z`), heading `facing` radians (counter-clockwise from the
/// field's +x axis seen from above, `atan2(dy, dx)` of its movement: the protocol's `facing`),
/// with one model unit being `scale` walkmesh units ([`default_model_scale`]). This is the one
/// place the axis swap lives: native `(x, y, z)` (left, down, backwards) becomes
/// `(-z, x, -y)` (forward, left, up) before the heading, scale and translation are applied.
/// A proper rotation: triangle winding is preserved.
pub fn model_to_field(position: [f32; 3], facing: f32, scale: f32) -> [f32; 16] {
    // columns: native x -> (0, 1, 0), native y -> (0, 0, -1), native z -> (-1, 0, 0)
    let axes: [f32; 16] = [0.0, 1.0, 0.0, 0.0, 0.0, 0.0, -1.0, 0.0, -1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0];
    mat4_mul(&mat4_translate(position), &mat4_mul(&mat4_rotate_z(facing), &mat4_mul(&mat4_scale(scale), &axes)))
}

/// [`DEFAULT_MODEL_SCALE_UNITS`] adjusted for a model scale (`512` is 1.0): pass the field's own
/// scale ([`crate::field::ModelLoaderHeader::scale`]) for a standard character standing in that
/// field. Coarser-numbered fields are built in finer walkmesh units, so a character is larger in them
/// (the direction in which the field speeds scale too).
///
/// Why the field's scale: every model record of a field's loader section carries its own scale
/// ([`crate::field::LoaderModel::scale`], ASCII digits) and that, not the field's, is probably what
/// sizes the model in the game; but for the playable characters it equals the field's scale in 98% of
/// the 728 records of Cloud's skeleton (712; 519 of Tifa's 529, 310 of Barret's 315, 215 of Aerith's
/// 220; the rest are special scenes), whereas for other models it varies from 32 to 5 120 (a
/// small or large NPC; 56 of the 71 distinct (field, model) pairs differ). So the field's scale is
/// the scale of a normal-sized character, and a loader record with another value would, if that
/// reading is right, be sized with `default_model_scale(record.scale)`.
///
/// Checked against the art in two fields (`tests/real_data.rs`,
/// `model_scale_gives_cloud_a_plausible_size_in_md1stin_and_md1_1`): at the spawn point nearest the
/// camera Cloud is 57 pixels tall in `md1stin` (512) and 41 in `md1_1` (1024, a high camera; 20 pixels,
/// a character knee-high to the railings, if the factor were not doubled there). **UNVERIFIED**, like
/// the base factor. A zero scale counts as 512.
pub fn default_model_scale(scale: u16) -> f32 {
    let scale = if scale == 0 { crate::field::FIELD_SCALE_ONE } else { scale };
    DEFAULT_MODEL_SCALE_UNITS * f32::from(scale) / f32::from(crate::field::FIELD_SCALE_ONE)
}

// ---------------------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------------------

/// Read one archive entry, charging it against the cumulative budget.
fn read_entry<R: Read + Seek>(
    lgp: &mut LgpArchive<R>,
    kind: &'static str,
    name: &str,
    budget: &mut u64,
) -> Result<Vec<u8>, ModelError> {
    let bytes = match lgp.read(name) {
        Ok(bytes) => bytes,
        Err(LgpError::NotFound(_)) => return Err(ModelError::Missing { kind, name: name.to_string() }),
        Err(other) => return Err(other.into()),
    };
    let len = bytes.len() as u64;
    if len > *budget {
        return Err(ModelError::Budget { limit: MAX_MODEL_LOAD_BYTES });
    }
    *budget -= len;
    Ok(bytes)
}

/// Load the animation `name` (`aaff`, `AAFF.a` and `aaff.aki` all name the entry `aaff.a`).
pub fn load_field_animation<R: Read + Seek>(lgp: &mut LgpArchive<R>, name: &str) -> Result<FieldAnimation, ModelError> {
    let mut budget = MAX_MODEL_LOAD_BYTES;
    let bytes = read_entry(lgp, "animation", &format!("{}.a", entry_stem(name)), &mut budget)?;
    FieldAnimation::parse(&bytes)
}

/// The blend classification of a render state (see [`PartBlend`]).
fn classify_blend(state: &PRenderState) -> (PartBlend, f32) {
    if !state.blend {
        return (PartBlend::Opaque, 1.0);
    }
    let alpha = f32::from(state.alpha) / 255.0;
    // Direct3D blend factors: 5 = SRCALPHA, 2 = ONE
    if state.src_blend == 5 && state.dst_blend == 2 {
        (PartBlend::Additive, alpha)
    } else {
        (PartBlend::Alpha, alpha)
    }
}

/// Texture entries already loaded for one model (`None`: named but unusable).
type TextureCache = HashMap<String, Option<Arc<RgbaImage>>>;

#[allow(clippy::too_many_arguments)]
fn build_parts<R: Read + Seek>(
    lgp: &mut LgpArchive<R>,
    budget: &mut u64,
    bone_index: usize,
    pmodel: &PModel,
    rsd: &Rsd,
    cache: &mut TextureCache,
    parts: &mut Vec<MeshPart>,
    dropped: &mut usize,
    missing: &mut Vec<String>,
) -> Result<(), ModelError> {
    for (group_index, group) in pmodel.groups.iter().enumerate() {
        let resolved = pmodel.group_triangles(group);
        *dropped += resolved.dropped;
        if resolved.triangles.is_empty() {
            continue;
        }
        // The texture of a textured group: the RSD's `TEX[n]` for the group's texture number.
        let texture_name = if group.textured { rsd.textures.get(group.texture_number as usize).cloned() } else { None };
        let texture = match &texture_name {
            None => None,
            Some(name) => {
                if !cache.contains_key(name) {
                    let loaded = match read_entry(lgp, "texture", &format!("{name}.tex"), budget) {
                        Ok(bytes) => parse_tex(&bytes).ok().map(Arc::new),
                        Err(ModelError::Budget { limit }) => return Err(ModelError::Budget { limit }),
                        Err(_) => None,
                    };
                    if loaded.is_none() {
                        missing.push(name.clone());
                    }
                    cache.insert(name.clone(), loaded);
                }
                cache.get(name).cloned().flatten()
            }
        };

        let mut positions: Vec<[f32; 3]> = Vec::new();
        let mut normals: Vec<[f32; 3]> = Vec::new();
        let mut uvs: Vec<[f32; 2]> = Vec::new();
        let mut colors: Vec<[u8; 4]> = Vec::new();
        let mut indices: Vec<u32> = Vec::new();
        let mut seen: HashMap<(usize, Option<usize>, Option<usize>), u32> = HashMap::new();
        for triangle in &resolved.triangles {
            // The file's winding is clockwise seen from outside: swap corners 1 and 2.
            for corner in [triangle[0], triangle[2], triangle[1]] {
                let key = (corner.vertex, corner.normal, corner.texcoord);
                let index = match seen.get(&key) {
                    Some(&i) => i,
                    None => {
                        let i = u32::try_from(positions.len()).map_err(|_| ModelError::TooMany {
                            what: "mesh part vertices",
                            count: positions.len() as u64,
                            limit: u64::from(u32::MAX),
                        })?;
                        positions.push(pmodel.vertices.get(corner.vertex).copied().unwrap_or([0.0; 3]));
                        normals.push(corner.normal.and_then(|n| pmodel.normals.get(n)).copied().unwrap_or([0.0; 3]));
                        uvs.push(corner.texcoord.and_then(|t| pmodel.texcoords.get(t)).copied().unwrap_or([0.0; 2]));
                        colors.push(pmodel.vertex_colors.get(corner.vertex).copied().unwrap_or([255; 4]));
                        seen.insert(key, i);
                        i
                    }
                };
                indices.push(index);
            }
        }
        let (blend, alpha) = classify_blend(&pmodel.render_state(group_index));
        parts.push(MeshPart {
            bone_index,
            positions,
            normals,
            uvs,
            colors,
            indices,
            texture,
            texture_name,
            blend,
            alpha,
        });
    }
    Ok(())
}

/// Load the character whose skeleton is `hrc` (`aaaa`, `AAAA.HRC`, ...) from `char.lgp`:
/// the skeleton, then for each bone its RSD files, their `.p` meshes and the `.tex` textures of
/// the textured groups. A missing or malformed skeleton, RSD or mesh is an error; a missing or
/// malformed texture only leaves that part untextured (listed in
/// [`FieldModel::missing_textures`]).
pub fn load_field_model<R: Read + Seek>(lgp: &mut LgpArchive<R>, hrc: &str) -> Result<FieldModel, ModelError> {
    let mut budget = MAX_MODEL_LOAD_BYTES;
    let hrc_name = entry_stem(hrc);
    let hrc_bytes = read_entry(lgp, "skeleton", &format!("{hrc_name}.hrc"), &mut budget)?;
    let skeleton = Hrc::parse(&hrc_bytes)?;
    let bones: Vec<ModelBone> =
        skeleton.bones.iter().map(|b| ModelBone { name: b.name.clone(), parent: b.parent, length: b.length }).collect();

    let mut cache = TextureCache::new();
    let mut parts = Vec::new();
    let mut dropped = 0usize;
    let mut missing = Vec::new();
    for (bone_index, bone) in skeleton.bones.iter().enumerate() {
        for rsd_name in &bone.rsd {
            let rsd_bytes = read_entry(lgp, "rsd", &format!("{}.rsd", entry_stem(rsd_name)), &mut budget)?;
            let rsd = Rsd::parse(&rsd_bytes)?;
            let p_bytes = read_entry(lgp, "mesh", &format!("{}.p", rsd.p_name), &mut budget)?;
            let pmodel = PModel::parse(&p_bytes)?;
            build_parts(
                lgp,
                &mut budget,
                bone_index,
                &pmodel,
                &rsd,
                &mut cache,
                &mut parts,
                &mut dropped,
                &mut missing,
            )?;
        }
    }
    Ok(FieldModel {
        hrc_name,
        skeleton_name: skeleton.name,
        bones,
        parts,
        dropped_triangles: dropped,
        missing_textures: missing,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::anim::tests::build_anim;
    use crate::anim::AnimFrame;
    use crate::pmodel::tests::PSpec;
    use crate::testutil::build_lgp;
    use crate::tex::tests::TexSpec;
    use std::io::Cursor;

    /// A two-bone model in a synthetic char.lgp: bone 0 ("hip", length 2) has one RSD whose mesh
    /// has an untextured group and a textured one (the default `PSpec`, texture 1 of two);
    /// bone 1 ("head", child of hip, length 3) has an RSD with no textures and a mesh.
    pub(crate) fn synthetic_char_lgp() -> Vec<u8> {
        let hrc =
            ":HEADER_BLOCK 2\n:SKELETON test_sk\n:BONES 2\n\nhip\nroot\n2.0\n1 PARTA\n\nhead\nhip\n3.0\n1 PARTB\n";
        let rsd_a =
            "@RSD940102\nPLY=MESHA.PLY\nMAT=MESHA.MAT\nGRP=MESHA.GRP\nNTEX=2\nTEX[0]=SKIN.TIM\nTEX[1]=FACE.TIM\n";
        let rsd_b = "@RSD940102\nPLY=MESHB.PLY\nMAT=MESHB.MAT\nGRP=MESHB.GRP\nNTEX=0\n";
        let mesh_a = PSpec::default().build();
        let mesh_b = PSpec {
            groups: vec![(0, 1, 0, 3, 0, false, 0)],
            polygons: vec![([0, 1, 2], [0, 0, 0])],
            bounding_box: false,
            ..PSpec::default()
        }
        .build();
        let face = TexSpec::default().build();
        let skin = TexSpec {
            palette: vec![[0, 0, 0, 255], [5, 5, 5, 255], [6, 6, 6, 255], [7, 7, 7, 255]],
            key_flag: false,
            ..TexSpec::default()
        }
        .build();
        let anim = build_anim(
            &[AnimFrame { root_rotation: [0.0; 3], root_translation: [0.0, 5.0, 0.0], rotations: vec![[0.0; 3]; 2] }],
            2,
            [1, 0, 2],
        );
        let anim3 = build_anim(
            &[AnimFrame { root_rotation: [0.0; 3], root_translation: [0.0; 3], rotations: vec![[0.0; 3]; 3] }],
            3,
            [1, 0, 2],
        );
        build_lgp(&[
            ("TEST.HRC", hrc.as_bytes()),
            ("PARTA.RSD", rsd_a.as_bytes()),
            ("PARTB.RSD", rsd_b.as_bytes()),
            ("MESHA.P", &mesh_a),
            ("MESHB.P", &mesh_b),
            ("SKIN.TEX", &skin),
            ("FACE.TEX", &face),
            ("WALK.A", &anim),
            ("ODD.A", &anim3),
        ])
    }

    fn load(bytes: Vec<u8>, name: &str) -> Result<FieldModel, ModelError> {
        load_field_model(&mut LgpArchive::open(Cursor::new(bytes)).unwrap(), name)
    }

    #[test]
    fn loads_a_synthetic_model_with_parts_textures_and_deduplicated_vertices() {
        let model = load(synthetic_char_lgp(), "test").unwrap();
        assert_eq!((model.hrc_name.as_str(), model.skeleton_name.as_str()), ("test", "test_sk"));
        assert_eq!(model.bones.len(), 2);
        assert_eq!((model.bones[0].parent, model.bones[0].length), (None, 2.0));
        assert_eq!((model.bones[1].parent, model.bones[1].length), (Some(0), 3.0));
        assert!(model.missing_textures.is_empty());
        assert_eq!(model.dropped_triangles, 0);
        // bone 0: the default mesh has two groups (textured with texture 1 = FACE, untextured);
        // bone 1: one untextured group
        assert_eq!(model.parts.len(), 3);
        let textured = &model.parts[0];
        assert_eq!((textured.bone_index, textured.texture_name.as_deref()), (0, Some("face")));
        assert!(textured.texture.is_some());
        assert_eq!(textured.triangle_count(), 2);
        // two triangles of the unit square share the diagonal: 4 distinct vertices, not 6
        assert_eq!(textured.positions.len(), 4);
        assert_eq!(textured.indices.len(), 6);
        assert!(textured.indices.iter().all(|&i| (i as usize) < textured.positions.len()));
        assert_eq!(textured.blend, PartBlend::Opaque);
        let plain = &model.parts[1];
        assert_eq!((plain.bone_index, plain.texture.is_none(), plain.texture_name.is_none()), (0, true, true));
        // group 1 of the default mesh is the "blend" one of the synthetic hundreds (alpha 128)
        assert_eq!(plain.blend, PartBlend::Alpha);
        assert!((plain.alpha - 128.0 / 255.0).abs() < 1e-6);
        assert_eq!(model.parts[2].bone_index, 1);
        assert_eq!(model.triangle_count(), 2 + 1 + 1);
        assert!(format!("{:?}", model.parts[0]).contains("triangles"));
        // UVs come from the texcoord array through the group's base; colours are R, G, B, A
        let uv_of = |part: &MeshPart, position: [f32; 3]| {
            let i = part.positions.iter().position(|p| *p == position).unwrap();
            part.uvs[i]
        };
        assert_eq!(uv_of(textured, [10.0, 10.0, 0.0]), [1.0, 1.0]);
        assert_eq!(uv_of(textured, [0.0, 10.0, 0.0]), [0.0, 1.0]);
        let colour = textured.colors[textured.positions.iter().position(|p| *p == [0.0, 0.0, 0.0]).unwrap()];
        assert_eq!(colour, [3, 2, 1, 255]);
        // the winding is swapped: the file's first triangle (0, 1, 2) comes out as (0, 2, 1)
        let tri =
            |i: usize| -> [[f32; 3]; 3] { [0, 1, 2].map(|k| textured.positions[textured.indices[3 * i + k] as usize]) };
        assert_eq!(tri(0), [[0.0, 0.0, 0.0], [10.0, 10.0, 0.0], [10.0, 0.0, 0.0]]);
        // names are case-insensitive and extensions are ignored
        for name in ["TEST", "test.hrc", "Test.HRC", " test "] {
            assert_eq!(load(synthetic_char_lgp(), name).unwrap().parts.len(), 3, "{name}");
        }
    }

    #[test]
    fn loaded_models_and_animations_can_cross_threads() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<FieldModel>();
        assert_send_sync::<MeshPart>();
        assert_send_sync::<FieldAnimation>();
        assert_send_sync::<ModelError>();
        assert_send_sync::<crate::playable::Character>();
        // textures are shared, not copied, between a model and its clones
        let model = load(synthetic_char_lgp(), "test").unwrap();
        let shared = model.parts[0].texture.as_ref().unwrap();
        let copy = model.clone();
        assert!(Arc::ptr_eq(shared, copy.parts[0].texture.as_ref().unwrap()), "cloning a model shares its textures");
    }

    #[test]
    fn the_texture_of_a_group_is_picked_by_texture_number_and_shared_between_groups() {
        let model = load(synthetic_char_lgp(), "test").unwrap();
        let face = model.parts[0].texture.as_ref().unwrap();
        // FACE.TEX is the default TexSpec: entry 0 is transparent under the colour key
        assert_eq!(face.pixel(0, 0), Some([0, 0, 0, 0]));
        assert_eq!(face.pixel(1, 0), Some([10, 20, 30, 255]));
    }

    #[test]
    fn missing_pieces_are_errors_or_untextured_parts() {
        // a model that is not in the archive
        assert!(matches!(load(synthetic_char_lgp(), "nope"), Err(ModelError::Missing { kind: "skeleton", .. })));
        // a missing mesh or RSD is an error naming the entry
        let lgp = |skip: &str| {
            let full = synthetic_char_lgp();
            let a = LgpArchive::open(Cursor::new(full.clone())).unwrap();
            let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
            let mut r = LgpArchive::open(Cursor::new(full)).unwrap();
            for e in a.list() {
                if !e.name.eq_ignore_ascii_case(skip) {
                    entries.push((e.name.clone(), r.read(&e.name).unwrap()));
                }
            }
            let refs: Vec<(&str, &[u8])> = entries.iter().map(|(n, d)| (n.as_str(), d.as_slice())).collect();
            build_lgp(&refs)
        };
        assert!(
            matches!(load(lgp("MESHB.P"), "test"), Err(ModelError::Missing { kind: "mesh", name }) if name == "meshb.p")
        );
        assert!(matches!(load(lgp("PARTA.RSD"), "test"), Err(ModelError::Missing { kind: "rsd", .. })));
        // a missing texture leaves the part untextured and is reported
        let m = load(lgp("FACE.TEX"), "test").unwrap();
        assert!(m.parts[0].texture.is_none());
        assert_eq!(m.parts[0].texture_name.as_deref(), Some("face"));
        assert_eq!(m.missing_textures, ["face"]);
    }

    #[test]
    fn the_cumulative_read_budget_is_enforced() {
        let mut budget = 10u64;
        let mut lgp = LgpArchive::open(Cursor::new(synthetic_char_lgp())).unwrap();
        assert!(matches!(read_entry(&mut lgp, "mesh", "mesha.p", &mut budget), Err(ModelError::Budget { .. })));
        let mut budget = 1_000_000u64;
        let before = budget;
        let bytes = read_entry(&mut lgp, "mesh", "mesha.p", &mut budget).unwrap();
        assert_eq!(before - budget, bytes.len() as u64);
    }

    #[test]
    fn animations_load_by_any_spelling_and_fit_by_bone_count() {
        let bytes = synthetic_char_lgp();
        let mut lgp = LgpArchive::open(Cursor::new(bytes)).unwrap();
        let model = load_field_model(&mut lgp, "test").unwrap();
        for name in ["walk", "WALK.A", "walk.aki"] {
            let a = load_field_animation(&mut lgp, name).unwrap();
            assert!(model.fits(&a), "{name}");
        }
        let odd = load_field_animation(&mut lgp, "odd").unwrap();
        assert!(!model.fits(&odd));
        assert!(model.pose(&odd, 0).is_none());
        assert!(matches!(load_field_animation(&mut lgp, "none"), Err(ModelError::Missing { kind: "animation", .. })));
        // a posed model: bounds of the standing hip at height 5 (native y points down)
        let walk = load_field_animation(&mut lgp, "walk").unwrap();
        let pose = model.pose(&walk, 0).unwrap();
        let (lo, hi) = model.posed_bounds(&pose, &mat4_identity()).unwrap();
        assert!(lo[1] <= -5.0 && hi[1] >= -5.0, "{lo:?} {hi:?}");
        assert!(model.posed_bounds(&pose[..1], &mat4_identity()).is_none(), "a short pose cannot place every part");
    }

    #[test]
    fn model_to_field_maps_forward_left_and_up() {
        // native: x = left, y = down, z = backwards (the character faces -z)
        let at = |m: &[f32; 16], p: [f32; 3]| transform_point(m, p);
        let close = |a: [f32; 3], b: [f32; 3]| a.iter().zip(b.iter()).all(|(x, y)| (x - y).abs() < 1e-4);
        let m = model_to_field([0.0; 3], 0.0, 1.0);
        assert!(close(at(&m, [0.0, 0.0, -1.0]), [1.0, 0.0, 0.0]), "forward (-z) is the heading +x");
        assert!(close(at(&m, [0.0, -1.0, 0.0]), [0.0, 0.0, 1.0]), "up (-y) is +z");
        assert!(close(at(&m, [1.0, 0.0, 0.0]), [0.0, 1.0, 0.0]), "left (+x) is to the left of +x seen from above: +y");
        // heading, scale and translation
        let m = model_to_field([100.0, 200.0, 30.0], std::f32::consts::FRAC_PI_2, 2.0);
        assert!(close(at(&m, [0.0; 3]), [100.0, 200.0, 30.0]));
        assert!(close(at(&m, [0.0, 0.0, -1.0]), [100.0, 202.0, 30.0]), "facing +y at 90 degrees, scaled by 2");
        assert!(close(at(&m, [0.0, -1.0, 0.0]), [100.0, 200.0, 32.0]));
        // proper rotation: determinant of the 3x3 part is +1 (unit scale)
        let r = model_to_field([0.0; 3], 0.3, 1.0);
        let det = r[0] * (r[5] * r[10] - r[9] * r[6]) - r[4] * (r[1] * r[10] - r[9] * r[2])
            + r[8] * (r[1] * r[6] - r[5] * r[2]);
        assert!((det - 1.0).abs() < 1e-4, "{det}");
        // normals rotate but do not translate
        assert!(close(transform_vector(&m, [0.0, 0.0, -1.0]), [0.0, 2.0, 0.0]));
        // default scale follows the field scale
        assert_eq!(default_model_scale(512), DEFAULT_MODEL_SCALE_UNITS);
        assert_eq!(default_model_scale(1024), 2.0 * DEFAULT_MODEL_SCALE_UNITS);
        assert_eq!(default_model_scale(0), DEFAULT_MODEL_SCALE_UNITS);
    }

    #[test]
    fn matrix_helpers_compose_in_the_documented_order() {
        let t = mat4_translate([1.0, 2.0, 3.0]);
        let s = mat4_scale(2.0);
        // a * b applies b first
        assert_eq!(transform_point(&mat4_mul(&t, &s), [1.0, 1.0, 1.0]), [3.0, 4.0, 5.0]);
        assert_eq!(transform_point(&mat4_mul(&s, &t), [1.0, 1.0, 1.0]), [4.0, 6.0, 8.0]);
        assert_eq!(mat4_mul(&mat4_identity(), &t), t);
        assert_eq!(mat4_mul(&t, &mat4_identity()), t);
        let r = mat4_rotate_z(std::f32::consts::FRAC_PI_2);
        let p = transform_point(&r, [1.0, 0.0, 0.0]);
        assert!((p[0]).abs() < 1e-6 && (p[1] - 1.0).abs() < 1e-6 && p[2].abs() < 1e-6);
    }

    #[test]
    fn hostile_skeletons_and_meshes_are_errors_not_panics() {
        // a skeleton that names thousands of RSD files costs at most the budget
        let mut hrc = String::from(":BONES 1\nb\nroot\n1\n32");
        for _ in 0..32 {
            hrc.push_str(" MISSING");
        }
        hrc.push('\n');
        let lgp = build_lgp(&[("BAD.HRC", hrc.as_bytes())]);
        assert!(matches!(load(lgp, "bad"), Err(ModelError::Missing { kind: "rsd", .. })));
        // garbage in every slot
        let lgp = build_lgp(&[
            ("G.HRC", b"garbage"),
            ("H.HRC", b":BONES 1\na\nroot\n1\n1 X\n"),
            ("X.RSD", b"PLY=Y.PLY\n"),
            ("Y.P", &[7u8; 300]),
        ]);
        assert!(load(lgp.clone(), "g").is_err());
        assert!(load(lgp, "h").is_err());
    }
}
