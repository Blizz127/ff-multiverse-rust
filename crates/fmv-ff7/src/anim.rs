//! `.a`: field character animations (per-frame bone rotations and root motion), and the maths
//! that turns one frame plus a skeleton into per-bone matrices.
//!
//! **File layout verified on owned FF7 Steam Edition (en) on 2026-11-08** against all 3 209 `.a`
//! entries of `field/char.lgp` (see the ignored test `char_models_all_parse` in
//! `tests/real_data.rs`, which asserts the totals quoted here). Measured facts:
//!
//! * Every file has version 1, rotation order `(1, 0, 2)` and a zero unused byte; its length is
//!   exactly `36 + frames * (24 + 12 * bones)`. Frame counts run from 1 to 480 (30 in 987 files,
//!   15 in 294); bone counts are 21 (1 840 files; Cloud's `n_cloud_sk` has 21 bones), 23, 24,
//!   25, 28, 29 and others, and **0 in 130 files** (no per-bone data: a root-only clip).
//! * Rotations are **degrees** in `[0, 360)` in every file but one (`fghe.a`, 60 frames, 28 bones,
//!   holds absurd finite values up to 3.7e37: the loader sanitises them, see [`MAX_ANGLE`]).
//!   Root translations are small (hip height around 13 for Cloud) except in a few cut-scene
//!   clips (components up to about 1 100).
//!
//! # Sources
//!
//! * FFRTT "FF7/Field Module" <https://wiki.ffrtt.ru/index.php/FF7/Field_Module>, section on the
//!   `.a` format: 36-byte header (`version` 1, `frames_count`, `bones_count`, three rotation-order
//!   bytes where "0 means alpha rotation, 1 beta rotation and 2 gamma rotation", an unused byte,
//!   20 bytes of run-time data), then frames of `24 + 12 * bones` bytes: root rotation (3 x
//!   `f32`), root translation (3 x `f32`), one rotation (3 x `f32`) per bone.
//! * Makou Reactor `src/core/field/AFile.cpp` <https://github.com/myst6re/makoureactor>: the same
//!   layout and the check that the file holds every declared frame. Only layout facts were taken.
//!
//! # Posing a skeleton (the part no document states)
//!
//! The conventions below were found by posing Cloud's standing frame (`n_cloud_sk`, `acfe.a`)
//! under 96 candidate conventions (six axis orders, eight sign patterns for the three angles, bone
//! axis `-z` or `+z`) and scoring how physically plausible the figure is, then confirmed on his
//! walk (`aaff.a`) and run (`aaga.a`) and on the other playable characters. The search is
//! re-run by the ignored test `the_documented_pose_convention_is_the_only_physically_plausible_one`
//! and the confirmations are asserted by `cloud_poses_are_physically_plausible`, both in
//! `tests/real_data.rs`; `examples/model_dump.rs` draws the results.
//!
//! * **Bone axis.** A bone's mesh lies along `-z` of its own frame: for Cloud's forearm bones the
//!   mesh's `z` extent is exactly `[-length, 0]` (length 4.601, `z` from -4.60 to 0.00), the hand
//!   bones the same (length 2.279), and the mean `z` of every one of his 18 mesh parts is negative.
//!   So a child's origin is `(0, 0, -length)` from its parent's origin, in the parent's frame.
//! * **Rotation.** Each bone has Euler angles `(alpha, beta, gamma)` about x, y, z, in degrees. The
//!   rotation order bytes `(1, 0, 2)` name y, x, z: the matrix is
//!   `R = Ry(beta) * Rx(alpha) * Rz(gamma)` (the first byte names the outermost rotation, z is
//!   applied to the vertices first; Direct3D's yaw-pitch-roll), with the **standard right-handed**
//!   rotation matrices (`Rx = [[1,0,0],[0,c,-s],[0,s,c]]` and so on: no sign changes, the matrices
//!   of the PlayStation's libgte as the author remembers them, UNVERIFIED against its manual). The
//!   search's best two candidates are this one (score 1.33) and its left-right mirror image
//!   (the y and z angles negated, 1.32); the third is 1.80. The criteria are all of: the figure is
//!   upright, its face and its toes point the same way (horizontal cosine 0.75), the soles are flat
//!   (the four lowest vertices of each foot within 0.16 and 0.31 of each other) and level left to
//!   right (0.12), the legs point away from the head, and the legs are straight. The candidates
//!   that negate the x and z or the x and y angles also give an upright figure with arms hanging
//!   at the sides, but the toes point opposite to the face.
//! * **Which of the mirror pair.** The pair differ only in which side of the body the `l_*` limbs
//!   are on (the meshes are not mirrored, so the figures are not exact mirror images). The data
//!   cannot decide this alone; the documented sign pattern is chosen because it needs no sign
//!   change from the standard matrices and because it is the one in which the bone names agree
//!   with anatomy: the native frame is right-handed (see below), the character faces `-z` with up
//!   `-y`, so its left is `up x forward = +x`, and the `l_*` bones are at +x (`l_uparm` at
//!   x = +5.15, `r_uparm` at -5.13 in Cloud's idle frame). The chain-independent evidence that
//!   the stored frame is right-handed, so that "left" really is +x, is the head: Red XIII's scar
//!   (over his right eye in the game, as the author remembers it) is in the face texture, which does not depend on the sign
//!   pattern, and it lands on the viewer's left, which is -x, in the front view of `model_dump`.
//!   The limbs then agree with their names: Barret's gun (a grey metal hand against the brown
//!   fist of the other arm) is on his `r_*` chain, Cloud's pauldron and bracer are on his `l_*`
//!   arm and Vincent's claw on his `l_*` arm, as in the game
//!   (`the_asymmetric_limbs_are_on_the_sides_their_bone_names_say` asserts the first two). If the
//!   mirror pair's other member were the true one, the `l_*` names would label the character's
//!   right side.
//! * **Child placement.** `W(child) = W(parent) * T(0, 0, -length(parent)) * R(child)`, bones
//!   attached to `root` use `W(root) * R(child)`, and `W(root) = T(root translation) * R(root
//!   rotation)`. The first two are exercised by every pose above; **the root rotation is
//!   UNVERIFIED**: every clip the conventions were checked on (the standing, walking and running
//!   clips of the playable characters) has a zero root rotation, while 23 357 frames of 577 files
//!   have a non-zero one, so both its pivot (here the root translation point, the hip's chain
//!   origin) and its composition (here the same y, x, z Euler matrix as a bone's, applied before
//!   the translation) are assumptions.
//! * **Native model space.** In these conventions the figure is **upright along -y** (a
//!   right-handed frame with y pointing *down*, as on the PlayStation, whose data the PC files
//!   carry unchanged): the chest is above the hip at negative y, the character faces **-z** and its
//!   left side is **+x**. The root translation is the one exception to "as stored": its y is the
//!   hip's height *above* the ground (13.54 for Cloud standing, between 12.82 and 13.65 during the
//!   walk, highest at mid-stance), so it is applied as `-y` here. With that sign the lowest foot
//!   point stays within 0.6 units (0.46 to 1.06 below the ground line, a spread of 0.60) through
//!   all 30 frames of `aaff.a`; added the other way round it moves by 1.57. `x` and `z` of the
//!   translation are applied as stored (they are 0 and 0.08 in every locomotion clip, so their
//!   sign is **unverified**). [`FieldAnimation::pose`] returns matrices in this space; the single
//!   documented step into field (walkmesh) coordinates is [`crate::model::model_to_field`].
//!
//! # Safety against untrusted input
//!
//! The declared frame and bone counts are checked against the file length (checked `u64`
//! arithmetic) before anything is allocated, so memory is bounded by the file size. Non-finite
//! floats become zero and angles beyond [`MAX_ANGLE`] degrees are treated as zero, so poses are
//! always finite.

use crate::model::{mat4_mul, mat4_translate, ModelBone, ModelError};
use crate::util::{u32_at, u8_at};

/// Bytes of the fixed header.
pub const ANIM_HEADER_LEN: usize = 36;
/// Most frames accepted in one animation (the real maximum is 480).
pub const MAX_ANIM_FRAMES: usize = 4096;
/// Most bones accepted in one animation (real files have at most 29).
pub const MAX_ANIM_BONES: usize = 128;
/// Largest angle (degrees) taken at face value. Real data lie in `[0, 360)`; anything with a
/// larger magnitude (one real file has values around 1e37) is treated as 0.
pub const MAX_ANGLE: f32 = 1.0e5;
/// Rotation order used when a file's three order bytes are not a permutation of 0, 1, 2: y, x, z
/// (the value of every real file).
pub const DEFAULT_ROTATION_ORDER: [u8; 3] = [1, 0, 2];

/// One frame: the root's rotation and translation and one rotation per bone, all as stored
/// (degrees, model units; see the module docs for the conventions).
#[derive(Debug, Clone, PartialEq)]
pub struct AnimFrame {
    pub root_rotation: [f32; 3],
    pub root_translation: [f32; 3],
    pub rotations: Vec<[f32; 3]>,
}

/// A decoded field animation.
#[derive(Clone, PartialEq)]
pub struct FieldAnimation {
    /// The three order bytes as stored (0 = rotation about x, 1 = y, 2 = z; the first names the
    /// outermost rotation). Every real file has `[1, 0, 2]`.
    pub rotation_order: [u8; 3],
    bone_count: usize,
    frames: Vec<AnimFrame>,
}

/// `Debug` prints counts only.
impl std::fmt::Debug for FieldAnimation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FieldAnimation")
            .field("frames", &self.frames.len())
            .field("bones", &self.bone_count)
            .field("rotation_order", &self.rotation_order)
            .finish()
    }
}

/// A finite angle in degrees: non-finite or absurdly large values count as 0.
fn clean_angle(v: f32) -> f32 {
    if v.is_finite() && v.abs() <= MAX_ANGLE {
        v
    } else {
        0.0
    }
}

fn clean_translation(v: f32) -> f32 {
    if v.is_finite() {
        v
    } else {
        0.0
    }
}

impl FieldAnimation {
    /// Parse a `.a` file.
    pub fn parse(bytes: &[u8]) -> Result<Self, ModelError> {
        if bytes.len() < ANIM_HEADER_LEN {
            return Err(ModelError::Truncated {
                what: "animation header",
                at: 0,
                needed: ANIM_HEADER_LEN - bytes.len(),
            });
        }
        let word = |off: usize| {
            u32_at(bytes, off).ok_or(ModelError::Truncated { what: "animation header", at: off, needed: 4 })
        };
        let version = word(0)?;
        if version != 1 {
            return Err(ModelError::Invalid { what: "animation", why: format!("version {version} (expected 1)") });
        }
        let frame_count = word(4)? as usize;
        let bone_count = word(8)? as usize;
        let rotation_order =
            [u8_at(bytes, 12).unwrap_or(1), u8_at(bytes, 13).unwrap_or(0), u8_at(bytes, 14).unwrap_or(2)];
        if frame_count == 0 {
            return Err(ModelError::Invalid { what: "animation", why: "no frames".into() });
        }
        if frame_count > MAX_ANIM_FRAMES {
            return Err(ModelError::TooMany {
                what: "animation frames",
                count: frame_count as u64,
                limit: MAX_ANIM_FRAMES as u64,
            });
        }
        if bone_count > MAX_ANIM_BONES {
            return Err(ModelError::TooMany {
                what: "animation bones",
                count: bone_count as u64,
                limit: MAX_ANIM_BONES as u64,
            });
        }
        // Both counts are capped, so these products cannot overflow u64.
        let frame_len = 24 + 12 * bone_count as u64;
        let needed = ANIM_HEADER_LEN as u64 + frame_count as u64 * frame_len;
        if needed > bytes.len() as u64 {
            return Err(ModelError::Truncated {
                what: "animation frames",
                at: bytes.len(),
                needed: usize::try_from(needed - bytes.len() as u64).unwrap_or(usize::MAX),
            });
        }
        let float3 = |at: usize| -> Option<[f32; 3]> {
            let read = |k: usize| {
                let bits = u32_at(bytes, at.checked_add(4 * k)?)?;
                Some(f32::from_bits(bits))
            };
            Some([read(0)?, read(1)?, read(2)?])
        };
        let mut frames = Vec::with_capacity(frame_count);
        for f in 0..frame_count {
            let base = ANIM_HEADER_LEN + f * frame_len as usize;
            let truncated = || ModelError::Truncated { what: "animation frame", at: base, needed: frame_len as usize };
            let root_rotation = float3(base).ok_or_else(truncated)?.map(clean_angle);
            let root_translation = float3(base + 12).ok_or_else(truncated)?.map(clean_translation);
            let mut rotations = Vec::with_capacity(bone_count);
            for b in 0..bone_count {
                rotations.push(float3(base + 24 + 12 * b).ok_or_else(truncated)?.map(clean_angle));
            }
            frames.push(AnimFrame { root_rotation, root_translation, rotations });
        }
        Ok(Self { rotation_order, bone_count, frames })
    }

    /// Number of frames (at least 1).
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// Number of bones this animation drives. An animation fits a skeleton when the counts match.
    pub fn bone_count(&self) -> usize {
        self.bone_count
    }

    /// Frame `index`, if it exists.
    pub fn frame(&self, index: usize) -> Option<&AnimFrame> {
        self.frames.get(index)
    }

    /// All frames.
    pub fn frames(&self) -> &[AnimFrame] {
        &self.frames
    }

    /// Whether this animation can drive `bones`: the bone counts match, or this is a root-only
    /// clip (no per-bone data, 130 of the owned files) and the skeleton is a single-bone "null"
    /// model (105 owned skeletons, see [`crate::hrc::Hrc::declared_bones`]), whose one bone then
    /// just follows the root rotation and translation.
    pub fn fits(&self, bones: &[ModelBone]) -> bool {
        bones.len() == self.bone_count || (self.bone_count == 0 && bones.len() == 1)
    }

    /// Per-bone matrices for frame `frame` (taken modulo the frame count, so a running counter
    /// loops): `result[i]` maps bone `i`'s own coordinates (the vertices of its meshes) into the
    /// native model space (see the module docs; use [`crate::model::model_to_field`] to place it
    /// in the field). Column-major, usable as `glam::Mat4::from_cols_array`. `None` if the
    /// animation does not fit `bones`.
    pub fn pose(&self, bones: &[ModelBone], frame: usize) -> Option<Vec<[f32; 16]>> {
        if !self.fits(bones) || self.frames.is_empty() {
            return None;
        }
        let f = self.frames.get(frame % self.frames.len())?;
        Some(pose_frame(f, self.rotation_order, bones))
    }

    /// Like [`FieldAnimation::pose`] for a fractional frame position: the angles and the root
    /// translation are interpolated linearly between the two neighbouring frames (angles along the
    /// shorter way round), looping at the end. Useful to pace a clip at a rate other than 30 frames
    /// per second. A non-finite `time` is frame 0.
    pub fn pose_at(&self, bones: &[ModelBone], time: f32) -> Option<Vec<[f32; 16]>> {
        if !self.fits(bones) || self.frames.is_empty() {
            return None;
        }
        let n = self.frames.len();
        let time = if time.is_finite() { time.rem_euclid(n as f32) } else { 0.0 };
        let lower = (time.floor() as usize).min(n - 1);
        let t = time - time.floor();
        let a = self.frames.get(lower)?;
        let b = self.frames.get((lower + 1) % n)?;
        Some(pose_frame(&lerp_frame(a, b, t), self.rotation_order, bones))
    }
}

/// Linear interpolation of two frames (shorter way round for angles).
fn lerp_frame(a: &AnimFrame, b: &AnimFrame, t: f32) -> AnimFrame {
    let angle = |x: f32, y: f32| {
        let d = (y - x + 540.0).rem_euclid(360.0) - 180.0;
        x + d * t
    };
    let angles3 = |x: [f32; 3], y: [f32; 3]| [angle(x[0], y[0]), angle(x[1], y[1]), angle(x[2], y[2])];
    let lerp3 =
        |x: [f32; 3], y: [f32; 3]| [x[0] + (y[0] - x[0]) * t, x[1] + (y[1] - x[1]) * t, x[2] + (y[2] - x[2]) * t];
    AnimFrame {
        root_rotation: angles3(a.root_rotation, b.root_rotation),
        root_translation: lerp3(a.root_translation, b.root_translation),
        rotations: a.rotations.iter().zip(b.rotations.iter()).map(|(x, y)| angles3(*x, *y)).collect(),
    }
}

/// Rotation matrix about one axis (0 = x, 1 = y, 2 = z), standard right-handed, `degrees`.
fn axis_rotation(axis: u8, degrees: f32) -> [f32; 16] {
    let (s, c) = degrees.to_radians().sin_cos();
    match axis {
        0 => [1.0, 0.0, 0.0, 0.0, 0.0, c, s, 0.0, 0.0, -s, c, 0.0, 0.0, 0.0, 0.0, 1.0],
        1 => [c, 0.0, -s, 0.0, 0.0, 1.0, 0.0, 0.0, s, 0.0, c, 0.0, 0.0, 0.0, 0.0, 1.0],
        _ => [c, s, 0.0, 0.0, -s, c, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0],
    }
}

/// The rotation matrix of Euler angles `(alpha, beta, gamma)` (about x, y, z) composed in
/// `order` (the first byte names the outermost rotation: `[1, 0, 2]` is `Ry * Rx * Rz`). An order
/// that is not a permutation of 0, 1, 2 is replaced by [`DEFAULT_ROTATION_ORDER`].
pub fn euler_matrix(angles: [f32; 3], order: [u8; 3]) -> [f32; 16] {
    let mut seen = [false; 3];
    for &o in &order {
        if let Some(slot) = seen.get_mut(usize::from(o)) {
            *slot = true;
        }
    }
    let order = if seen.iter().all(|&s| s) { order } else { DEFAULT_ROTATION_ORDER };
    let [a, b, c] = order;
    let angle_of = |axis: u8| angles.get(usize::from(axis)).copied().unwrap_or(0.0);
    mat4_mul(&axis_rotation(a, angle_of(a)), &mat4_mul(&axis_rotation(b, angle_of(b)), &axis_rotation(c, angle_of(c))))
}

/// The per-bone matrices of one frame (see [`FieldAnimation::pose`]); `frame.rotations` must have
/// one entry per bone (missing entries count as no rotation). Parents are evaluated before their
/// children in one pass, as the skeleton format guarantees; a bone whose parent index is not
/// smaller than its own is treated as attached to the root.
pub fn pose_frame(frame: &AnimFrame, order: [u8; 3], bones: &[ModelBone]) -> Vec<[f32; 16]> {
    // The root translation's y is a height above the ground; the native frame's y points down.
    let [tx, ty, tz] = frame.root_translation;
    let root = mat4_mul(&mat4_translate([tx, -ty, tz]), &euler_matrix(frame.root_rotation, order));
    let mut world: Vec<[f32; 16]> = Vec::with_capacity(bones.len());
    for (i, bone) in bones.iter().enumerate() {
        let local = euler_matrix(frame.rotations.get(i).copied().unwrap_or([0.0; 3]), order);
        let parent = bone.parent.filter(|&p| p < i).and_then(|p| world.get(p).zip(bones.get(p)));
        let matrix = match parent {
            Some((parent_world, parent_bone)) => {
                mat4_mul(&mat4_mul(parent_world, &mat4_translate([0.0, 0.0, -parent_bone.length])), &local)
            }
            None => mat4_mul(&root, &local),
        };
        world.push(matrix);
    }
    world
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::model::transform_point;

    /// Build a `.a` file in the documented layout (independent of the parser).
    pub(crate) fn build_anim(frames: &[AnimFrame], bones: usize, order: [u8; 3]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&(frames.len() as u32).to_le_bytes());
        out.extend_from_slice(&(bones as u32).to_le_bytes());
        out.extend_from_slice(&order);
        out.push(0); // unused
        out.extend(std::iter::repeat_n(0xCDu8, 20)); // run-time data
        for f in frames {
            for v in f.root_rotation.iter().chain(f.root_translation.iter()) {
                out.extend_from_slice(&v.to_le_bytes());
            }
            for r in &f.rotations {
                for v in r {
                    out.extend_from_slice(&v.to_le_bytes());
                }
            }
        }
        out
    }

    fn bone(parent: Option<usize>, length: f32) -> ModelBone {
        ModelBone { name: String::new(), parent, length }
    }

    fn close(a: [f32; 3], b: [f32; 3]) -> bool {
        a.iter().zip(b.iter()).all(|(x, y)| (x - y).abs() < 1e-4)
    }

    #[test]
    fn parses_the_documented_layout() {
        let frames = vec![
            AnimFrame {
                root_rotation: [1.0, 2.0, 3.0],
                root_translation: [4.0, 5.0, 6.0],
                rotations: vec![[10.0, 20.0, 30.0], [40.0, 50.0, 60.0]],
            },
            AnimFrame {
                root_rotation: [0.0; 3],
                root_translation: [0.0, 13.5, 0.08],
                rotations: vec![[270.0, 0.0, 0.0], [350.0, 0.0, 0.0]],
            },
        ];
        let bytes = build_anim(&frames, 2, [1, 0, 2]);
        assert_eq!(bytes.len(), 36 + 2 * (24 + 24));
        let a = FieldAnimation::parse(&bytes).unwrap();
        assert_eq!((a.frame_count(), a.bone_count(), a.rotation_order), (2, 2, [1, 0, 2]));
        assert_eq!(a.frames(), frames.as_slice());
        assert_eq!(a.frame(1), frames.get(1));
        assert_eq!(a.frame(2), None);
        assert!(format!("{a:?}").contains("frames"));
        // a bone-less clip is valid (130 real files are)
        let rootless = build_anim(
            &[AnimFrame { root_rotation: [0.0; 3], root_translation: [1.0, 2.0, 3.0], rotations: vec![] }],
            0,
            [1, 0, 2],
        );
        let r = FieldAnimation::parse(&rootless).unwrap();
        assert_eq!((r.frame_count(), r.bone_count()), (1, 0));
        assert!(r.fits(&[]));
        // trailing bytes are ignored
        let mut padded = bytes.clone();
        padded.extend_from_slice(&[9; 17]);
        assert_eq!(FieldAnimation::parse(&padded).unwrap(), a);
    }

    #[test]
    fn rejects_bad_headers_and_counts_before_allocating() {
        let good = build_anim(
            &[AnimFrame { root_rotation: [0.0; 3], root_translation: [0.0; 3], rotations: vec![[0.0; 3]] }],
            1,
            [1, 0, 2],
        );
        assert!(matches!(FieldAnimation::parse(&[]), Err(ModelError::Truncated { .. })));
        assert!(matches!(FieldAnimation::parse(good.get(..35).unwrap()), Err(ModelError::Truncated { .. })));
        assert!(matches!(
            FieldAnimation::parse(good.get(..good.len() - 1).unwrap()),
            Err(ModelError::Truncated { .. })
        ));
        let poke = |off: usize, v: u32| {
            let mut b = good.clone();
            b[off..off + 4].copy_from_slice(&v.to_le_bytes());
            FieldAnimation::parse(&b)
        };
        assert!(matches!(poke(0, 2), Err(ModelError::Invalid { .. })), "version");
        assert!(matches!(poke(4, 0), Err(ModelError::Invalid { .. })), "no frames");
        assert!(matches!(poke(4, u32::MAX), Err(ModelError::TooMany { .. })));
        assert!(matches!(poke(4, MAX_ANIM_FRAMES as u32 + 1), Err(ModelError::TooMany { .. })));
        assert!(matches!(poke(8, MAX_ANIM_BONES as u32 + 1), Err(ModelError::TooMany { .. })));
        assert!(matches!(poke(8, u32::MAX), Err(ModelError::TooMany { .. })));
        // counts inside the caps but larger than the file
        assert!(matches!(poke(4, 4000), Err(ModelError::Truncated { .. })));
        assert!(matches!(poke(8, 100), Err(ModelError::Truncated { .. })));
    }

    #[test]
    fn sanitises_non_finite_and_absurd_values() {
        let f = AnimFrame {
            root_rotation: [f32::NAN, 1.0e37, -3.7e37],
            root_translation: [f32::INFINITY, 2.0, f32::NEG_INFINITY],
            rotations: vec![[f32::NAN, 90.0, 1.0e6]],
        };
        let a = FieldAnimation::parse(&build_anim(&[f], 1, [1, 0, 2])).unwrap();
        let g = a.frame(0).unwrap();
        assert_eq!(g.root_rotation, [0.0, 0.0, 0.0]);
        assert_eq!(g.root_translation, [0.0, 2.0, 0.0]);
        assert_eq!(g.rotations[0], [0.0, 90.0, 0.0]);
        let pose = a.pose(&[bone(None, 1.0)], 0).unwrap();
        assert!(pose.iter().flatten().all(|v| v.is_finite()));
    }

    #[test]
    fn euler_matrices_follow_the_rotation_order() {
        // order (1, 0, 2) = Ry * Rx * Rz: z is applied to a vertex first
        let m = euler_matrix([90.0, 90.0, 90.0], [1, 0, 2]);
        // (1, 0, 0) --Rz(90)--> (0, 1, 0) --Rx(90)--> (0, 0, 1) --Ry(90)--> (1, 0, 0)
        assert!(close(transform_point(&m, [1.0, 0.0, 0.0]), [1.0, 0.0, 0.0]));
        // (0, 0, 1) --Rz--> (0, 0, 1) --Rx(90)--> (0, -1, 0) --Ry--> (0, -1, 0)
        assert!(close(transform_point(&m, [0.0, 0.0, 1.0]), [0.0, -1.0, 0.0]));
        // the opposite order z, x, y gives a different matrix
        let other = euler_matrix([90.0, 90.0, 90.0], [2, 0, 1]);
        assert!(!close(transform_point(&other, [0.0, 0.0, 1.0]), [0.0, -1.0, 0.0]));
        // single-axis rotations are the standard right-handed ones
        assert!(close(transform_point(&euler_matrix([90.0, 0.0, 0.0], [1, 0, 2]), [0.0, 1.0, 0.0]), [0.0, 0.0, 1.0]));
        assert!(close(transform_point(&euler_matrix([0.0, 90.0, 0.0], [1, 0, 2]), [0.0, 0.0, 1.0]), [1.0, 0.0, 0.0]));
        assert!(close(transform_point(&euler_matrix([0.0, 0.0, 90.0], [1, 0, 2]), [1.0, 0.0, 0.0]), [0.0, 1.0, 0.0]));
        // an order that is not a permutation falls back to y, x, z instead of misbehaving
        assert_eq!(
            euler_matrix([10.0, 20.0, 30.0], [0, 0, 0]),
            euler_matrix([10.0, 20.0, 30.0], DEFAULT_ROTATION_ORDER)
        );
        assert_eq!(
            euler_matrix([10.0, 20.0, 30.0], [9, 1, 2]),
            euler_matrix([10.0, 20.0, 30.0], DEFAULT_ROTATION_ORDER)
        );
        // zero angles are the identity
        assert_eq!(euler_matrix([0.0; 3], [1, 0, 2]), crate::model::mat4_identity());
    }

    #[test]
    fn a_chain_is_placed_along_minus_z_and_rotations_accumulate() {
        // a three-bone chain; zero rotations: a straight line down -z, root at the translation
        let bones = [bone(None, 2.0), bone(Some(0), 3.0), bone(Some(1), 4.0)];
        let flat =
            AnimFrame { root_rotation: [0.0; 3], root_translation: [1.0, 0.0, 5.0], rotations: vec![[0.0; 3]; 3] };
        let pose = pose_frame(&flat, [1, 0, 2], &bones);
        let origin = |m: &[f32; 16]| transform_point(m, [0.0, 0.0, 0.0]);
        assert!(close(origin(&pose[0]), [1.0, 0.0, 5.0]));
        assert!(
            close(origin(&pose[1]), [1.0, 0.0, 3.0]),
            "child starts at the far end of its parent (-length along z)"
        );
        assert!(close(origin(&pose[2]), [1.0, 0.0, 0.0]));
        // the root translation's y is a height: applied as -y in the native (y down) frame
        let lifted = AnimFrame { root_translation: [0.0, 13.5, 0.0], ..flat.clone() };
        assert!(close(origin(&pose_frame(&lifted, [1, 0, 2], &bones)[0]), [0.0, -13.5, 0.0]));
        // rotating the first bone by 90 degrees about x turns -z into +y (right-handed Rx):
        // Rx(90) maps (0, 0, -1) to (0, 1, 0)
        let bent = AnimFrame {
            rotations: vec![[90.0, 0.0, 0.0], [0.0; 3], [0.0; 3]],
            root_translation: [0.0; 3],
            ..flat.clone()
        };
        let pose = pose_frame(&bent, [1, 0, 2], &bones);
        assert!(close(origin(&pose[1]), [0.0, 2.0, 0.0]));
        assert!(close(origin(&pose[2]), [0.0, 5.0, 0.0]), "the rotation carries down the chain");
        // a second rotation about x in the child (-90) straightens it again: -z from the joint
        let kink = AnimFrame { rotations: vec![[90.0, 0.0, 0.0], [270.0, 0.0, 0.0], [0.0; 3]], ..bent.clone() };
        let pose = pose_frame(&kink, [1, 0, 2], &bones);
        assert!(close(origin(&pose[2]), [0.0, 2.0, -3.0]));
        // the root rotation turns everything about the root translation
        let spun = AnimFrame {
            root_rotation: [0.0, 0.0, 90.0],
            root_translation: [10.0, 0.0, 0.0],
            rotations: vec![[0.0; 3]; 3],
        };
        let pose = pose_frame(&spun, [1, 0, 2], &bones);
        assert!(close(origin(&pose[0]), [10.0, 0.0, 0.0]));
        assert!(close(origin(&pose[1]), [10.0, 0.0, -2.0]), "a rotation about z leaves the z axis alone");
        // root-attached siblings both start at the root
        let two_roots = [bone(None, 1.0), bone(None, 1.0)];
        let p = pose_frame(&flat, [1, 0, 2], &two_roots);
        assert!(close(origin(&p[0]), origin(&p[1])));
        // a bad parent index (forward reference) attaches to the root instead of panicking
        let bad = [bone(Some(1), 1.0), bone(Some(7), 1.0)];
        assert_eq!(pose_frame(&flat, [1, 0, 2], &bad).len(), 2);
    }

    #[test]
    fn a_root_only_clip_drives_a_single_bone_null_model() {
        let frames = vec![
            AnimFrame { root_rotation: [0.0, 0.0, 90.0], root_translation: [1.0, 2.0, 3.0], rotations: vec![] },
            AnimFrame { root_rotation: [0.0; 3], root_translation: [4.0, 5.0, 6.0], rotations: vec![] },
        ];
        let a = FieldAnimation::parse(&build_anim(&frames, 0, [1, 0, 2])).unwrap();
        let one = [bone(None, 1.0)];
        assert!(a.fits(&one));
        assert!(a.fits(&[]));
        assert!(!a.fits(&[bone(None, 1.0), bone(Some(0), 1.0)]), "a real skeleton needs per-bone data");
        let pose = a.pose(&one, 0).unwrap();
        assert_eq!(pose.len(), 1);
        // the null bone sits at the root translation (y is a height: -y natively) and turns with it
        assert!(close(transform_point(&pose[0], [0.0; 3]), [1.0, -2.0, 3.0]));
        assert!(close(transform_point(&pose[0], [1.0, 0.0, 0.0]), [1.0, -1.0, 3.0]), "Rz(90) turns +x into +y (down)");
        assert!(close(transform_point(&a.pose(&one, 1).unwrap()[0], [0.0; 3]), [4.0, -5.0, 6.0]));
        // a one-bone clip drives the same model too
        let b = FieldAnimation::parse(&build_anim(
            &[AnimFrame { root_rotation: [0.0; 3], root_translation: [0.0; 3], rotations: vec![[0.0; 3]] }],
            1,
            [1, 0, 2],
        ))
        .unwrap();
        assert!(b.fits(&one));
    }

    #[test]
    fn pose_checks_the_bone_count_and_loops_over_frames() {
        let frames: Vec<AnimFrame> = (0..3)
            .map(|i| AnimFrame {
                root_rotation: [0.0; 3],
                root_translation: [i as f32, 0.0, 0.0],
                rotations: vec![[0.0; 3]; 2],
            })
            .collect();
        let a = FieldAnimation::parse(&build_anim(&frames, 2, [1, 0, 2])).unwrap();
        let bones = [bone(None, 1.0), bone(Some(0), 1.0)];
        assert!(a.fits(&bones));
        assert!(a.pose(&bones[..1], 0).is_none(), "wrong bone count");
        assert!(a.pose_at(&bones[..1], 0.0).is_none());
        let x = |p: Vec<[f32; 16]>| transform_point(&p[0], [0.0; 3])[0];
        assert_eq!(x(a.pose(&bones, 1).unwrap()), 1.0);
        assert_eq!(x(a.pose(&bones, 4).unwrap()), 1.0, "frames loop");
        assert_eq!(x(a.pose(&bones, usize::MAX).unwrap()), (usize::MAX % 3) as f32);
        // fractional positions interpolate and loop (frame 2 -> frame 0)
        assert!((x(a.pose_at(&bones, 0.5).unwrap()) - 0.5).abs() < 1e-5);
        assert!((x(a.pose_at(&bones, 2.5).unwrap()) - 1.0).abs() < 1e-5, "halfway from x = 2 back to x = 0");
        assert_eq!(x(a.pose_at(&bones, f32::NAN).unwrap()), 0.0);
        assert_eq!(x(a.pose_at(&bones, -1.0).unwrap()), 2.0);
        assert!(x(a.pose_at(&bones, 1e30).unwrap()).is_finite());
    }

    #[test]
    fn angle_interpolation_takes_the_short_way_round() {
        let mk = |deg: f32| AnimFrame {
            root_rotation: [0.0; 3],
            root_translation: [0.0; 3],
            rotations: vec![[0.0, deg, 0.0]],
        };
        let a = FieldAnimation::parse(&build_anim(&[mk(350.0), mk(10.0)], 1, [1, 0, 2])).unwrap();
        let bones = [bone(None, 1.0)];
        // halfway between 350 and 10 degrees is 0 (not 180)
        let m = a.pose_at(&bones, 0.5).unwrap();
        let p = transform_point(&m[0], [0.0, 0.0, 1.0]);
        assert!(close(p, [0.0, 0.0, 1.0]), "{p:?}");
    }

    #[test]
    fn damaged_animations_never_panic() {
        let frames =
            vec![AnimFrame { root_rotation: [1.0; 3], root_translation: [2.0; 3], rotations: vec![[3.0; 3]; 2] }; 2];
        let good = build_anim(&frames, 2, [1, 0, 2]);
        let bones = [bone(None, 1.0), bone(Some(0), 1.0)];
        for n in 0..good.len() {
            let _ = FieldAnimation::parse(good.get(..n).unwrap_or(&[]));
        }
        for i in 0..good.len() {
            for flip in [0x01u8, 0x80, 0xFF] {
                let mut b = good.clone();
                b[i] ^= flip;
                if let Ok(a) = FieldAnimation::parse(&b) {
                    let _ = a.pose(&bones, 0);
                    let _ = a.pose_at(&bones, 0.7);
                }
            }
        }
    }
}
