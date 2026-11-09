//! A character's meshes merged into one vertex/index set, ready to skin on the CPU and draw in a
//! few batches.
//!
//! `fmv_ff7::FieldModel` keeps one [`MeshPart`] per `.p` group, each moved by one bone. Drawing
//! twenty-odd parts per player would mean twenty-odd draws and buffers, so [`MergedMesh::build`]
//! concatenates them: vertices stay grouped per part (so a part is one [`BoneRun`] and skinning is
//! one matrix per run), and the triangles are sorted into [`Batch`]es that need different state:
//!
//! 1. all opaque untextured triangles (almost everything: the vertex colours are the shading),
//! 2. the opaque textured parts (the face overlays: eyes, brows, mouth), one batch per texture,
//! 3. the semi-transparent parts (rare: 23 of 4 875 groups in the owned archive), one batch each,
//!    drawn last and without writing depth.
//!
//! Nothing here touches the GPU or any game file: it is a pure function of an already decoded
//! model, which is what makes the placement and batching unit-testable.

use super::identity::head_colour_is_own;
use fmv_ff7::{FieldModel, MeshPart, PartBlend, RgbaImage};
use std::sync::Arc;

/// The name of the bone of a field model's head (every skeleton of the owned `char.lgp` has it, third in the file).
const HEAD_BONE: &str = "head";

/// Vertices one merged model may have (a field model has a few thousand): a hostile or broken
/// archive cannot make the client ask the GPU for an absurd buffer.
pub const MAX_MODEL_VERTICES: usize = 200_000;

/// How a batch is blended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchKind {
    Opaque,
    /// Source-alpha blending with the vertex colours' alpha (the part's constant alpha).
    Alpha,
    /// Source-alpha scaled light added to the picture.
    Additive,
}

/// A run of triangles drawn with one texture and one blend state.
#[derive(Clone)]
pub struct Batch {
    pub kind: BatchKind,
    /// First index (into [`MergedMesh::indices`]) and how many.
    pub first_index: u32,
    pub index_count: u32,
    /// The texture its triangles sample (`None`: untextured, vertex colours only).
    pub texture: Option<Arc<RgbaImage>>,
}

/// The vertices `start..start + len` of a merged mesh belong to one bone (one part).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoneRun {
    pub bone: usize,
    pub start: usize,
    pub len: usize,
}

/// All parts of a model as one mesh. Arrays are per vertex; `indices` index them in threes.
#[derive(Clone)]
pub struct MergedMesh {
    /// Positions in the owning bone's own frame (model units, unscaled).
    pub positions: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    /// RGBA vertex colours. The alpha of a semi-transparent part's vertices is the part's constant
    /// alpha; everywhere else it is 255.
    pub colors: Vec<[u8; 4]>,
    /// 1.0 on the vertices of textured opaque parts, 0.0 elsewhere: those parts are overlays that
    /// lie on top of other polygons (a face on a head), so the renderer pulls them a hair towards
    /// the camera in the depth test instead of letting them fight with the skin underneath.
    pub overlay: Vec<f32>,
    /// 1.0 on the vertices that keep the model's own colours under a player's identity colour although they are not face
    /// decals: the hair (and the skin of the head, which is kept anyway), the vertices of the head bone whose colour is a
    /// hair or skin colour ([`super::identity::head_colour_is_own`]). 0.0 elsewhere, and on the decals (they keep theirs
    /// through `overlay`).
    pub own: Vec<f32>,
    pub bone_runs: Vec<BoneRun>,
    pub indices: Vec<u32>,
    pub batches: Vec<Batch>,
}

impl std::fmt::Debug for MergedMesh {
    /// Counts only: decoded game geometry stays out of logs.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MergedMesh")
            .field("vertices", &self.positions.len())
            .field("triangles", &(self.indices.len() / 3))
            .field("batches", &self.batches.len())
            .finish()
    }
}

/// Why a model cannot be merged.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MeshError {
    #[error("the model has no triangles")]
    Empty,
    #[error("the model has {0} vertices (at most {MAX_MODEL_VERTICES} are accepted)")]
    TooManyVertices(usize),
}

fn same_texture(a: &Arc<RgbaImage>, b: &Arc<RgbaImage>) -> bool {
    Arc::ptr_eq(a, b)
}

/// The triangles of `part` whose three indices exist, as indices into the merged arrays.
fn rebased(part: &MeshPart, base: u32) -> Vec<u32> {
    let n = part.positions.len();
    let mut out = Vec::with_capacity(part.indices.len());
    for tri in part.indices.chunks_exact(3) {
        if tri.iter().all(|&i| (i as usize) < n) {
            out.extend(tri.iter().map(|&i| base + i));
        }
    }
    out
}

/// How far past its cell's edge a part's coordinates may stray (the file's own rounding) and still count as being in it.
const CELL_SLACK: f32 = 0.03;

/// The whole numbers to subtract from a textured part's texture coordinates, per axis (`u`, `v`).
///
/// The models place a decal on a face by coordinates shifted by whole numbers: the right eye is the left eye's coordinates
/// plus 1 in `u` (its texture, `u` from 1.00 to 1.97, is the same eye), a mouth has 5 added to `v` (`v` from 5.00 to 5.94).
/// The game draws with wrap-around addressing, so a shift by a whole number is no shift at all. This window's sampler clamps
/// (so that a decal at the edge of its texture does not pick up the opposite edge when it is filtered), and a clamped
/// coordinate past 1 reads the transparent edge: **the second eye and the mouth of every field model were not drawn.** So
/// when all of a part's coordinates lie in one whole-number cell on an axis, that cell's number is what this returns: the
/// same picture the wrap gives, with the clamp's edges. A part whose coordinates spread over more than one cell (a texture
/// that really tiles) gets 0 on that axis: it is drawn as it always was.
fn whole_cell_offsets(uvs: &[[f32; 2]]) -> [f32; 2] {
    let mut out = [0.0f32; 2];
    for (axis, slot) in out.iter_mut().enumerate() {
        let (lo, hi) = uvs.iter().filter_map(|uv| uv.get(axis)).fold((f32::MAX, f32::MIN), |(lo, hi), &c| (lo.min(c), hi.max(c)));
        if !(lo.is_finite() && hi.is_finite()) || lo > hi {
            continue;
        }
        let cell = ((lo + hi) / 2.0).floor();
        if cell != 0.0 && cell.abs() < 1.0e6 && lo >= cell - CELL_SLACK && hi <= cell + 1.0 + CELL_SLACK {
            *slot = cell;
        }
    }
    out
}

impl MergedMesh {
    /// Merge the parts of `model`. Triangles with an out-of-range index are left out (the loader
    /// already drops them; this keeps the buffers sound whatever it is given). Parts that name a
    /// texture the loader could not read draw untextured. The whole-number shifts of a decal's
    /// texture coordinates are taken off (see [`whole_cell_offsets`]).
    pub fn build(model: &FieldModel) -> Result<Self, MeshError> {
        let total: usize = model.parts.iter().map(|p| p.positions.len()).sum();
        if total > MAX_MODEL_VERTICES {
            return Err(MeshError::TooManyVertices(total));
        }
        let mut mesh = MergedMesh {
            positions: Vec::with_capacity(total),
            uvs: Vec::with_capacity(total),
            colors: Vec::with_capacity(total),
            overlay: Vec::with_capacity(total),
            own: Vec::with_capacity(total),
            bone_runs: Vec::with_capacity(model.parts.len()),
            indices: Vec::new(),
            batches: Vec::new(),
        };
        let mut plain: Vec<u32> = Vec::new();
        let mut textured: Vec<(Arc<RgbaImage>, Vec<u32>)> = Vec::new();
        let mut blended: Vec<(BatchKind, Option<Arc<RgbaImage>>, Vec<u32>)> = Vec::new();

        for part in &model.parts {
            let len = part.positions.len();
            // Every per-vertex array of a part has `len` entries (the loader builds them together);
            // pad or cut defensively so the merged arrays stay parallel.
            let base = u32::try_from(mesh.positions.len()).map_err(|_| MeshError::TooManyVertices(total))?;
            let is_blended = part.blend != PartBlend::Opaque;
            let alpha = if is_blended { (part.alpha.clamp(0.0, 1.0) * 255.0).round() as u8 } else { 255 };
            let overlay = if !is_blended && part.texture.is_some() { 1.0 } else { 0.0 };
            let shift = if part.texture.is_some() { whole_cell_offsets(&part.uvs) } else { [0.0; 2] };
            // The hair is on the head bone, drawn from vertex colours (a decal is flagged by `overlay` already).
            let on_head = !is_blended && part.texture.is_none() && model.bones.get(part.bone_index).is_some_and(|bone| bone.name.eq_ignore_ascii_case(HEAD_BONE));
            for i in 0..len {
                mesh.positions.push(part.positions.get(i).copied().unwrap_or([0.0; 3]));
                let [u, v] = part.uvs.get(i).copied().unwrap_or([0.0; 2]);
                mesh.uvs.push([u - shift[0], v - shift[1]]);
                let [r, g, b, _] = part.colors.get(i).copied().unwrap_or([255; 4]);
                mesh.colors.push([r, g, b, alpha]);
                mesh.overlay.push(overlay);
                mesh.own.push(if on_head && head_colour_is_own([r, g, b]) { 1.0 } else { 0.0 });
            }
            mesh.bone_runs.push(BoneRun { bone: part.bone_index, start: base as usize, len });

            let tris = rebased(part, base);
            if tris.is_empty() {
                continue;
            }
            match (part.blend, &part.texture) {
                (PartBlend::Opaque, None) => plain.extend(tris),
                (PartBlend::Opaque, Some(tex)) => match textured.iter_mut().find(|(t, _)| same_texture(t, tex)) {
                    Some((_, list)) => list.extend(tris),
                    None => textured.push((Arc::clone(tex), tris)),
                },
                (PartBlend::Alpha, tex) => blended.push((BatchKind::Alpha, tex.clone(), tris)),
                (PartBlend::Additive, tex) => blended.push((BatchKind::Additive, tex.clone(), tris)),
            }
        }

        let push_batch = |mesh: &mut MergedMesh, kind: BatchKind, texture: Option<Arc<RgbaImage>>, tris: Vec<u32>| {
            let first_index = mesh.indices.len() as u32;
            let index_count = tris.len() as u32;
            mesh.indices.extend(tris);
            mesh.batches.push(Batch { kind, first_index, index_count, texture });
        };
        if !plain.is_empty() {
            push_batch(&mut mesh, BatchKind::Opaque, None, plain);
        }
        for (texture, tris) in textured {
            push_batch(&mut mesh, BatchKind::Opaque, Some(texture), tris);
        }
        for (kind, texture, tris) in blended {
            push_batch(&mut mesh, kind, texture, tris);
        }
        if mesh.indices.is_empty() {
            return Err(MeshError::Empty);
        }
        Ok(mesh)
    }

    /// Triangles over all batches.
    #[cfg(test)]
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use fmv_ff7::{FieldModel, ModelBone};

    pub(crate) fn part(bone: usize, tris: &[[u32; 3]], verts: usize) -> MeshPart {
        MeshPart {
            bone_index: bone,
            positions: (0..verts).map(|i| [i as f32, 0.0, 0.0]).collect(),
            normals: vec![[0.0; 3]; verts],
            uvs: vec![[0.0; 2]; verts],
            colors: vec![[200, 100, 50, 255]; verts],
            indices: tris.iter().flatten().copied().collect(),
            texture: None,
            texture_name: None,
            blend: PartBlend::Opaque,
            alpha: 1.0,
        }
    }

    pub(crate) fn model(parts: Vec<MeshPart>) -> FieldModel {
        FieldModel {
            hrc_name: "test".into(),
            skeleton_name: "test_sk".into(),
            bones: vec![
                ModelBone { name: "hip".into(), parent: None, length: 2.0 },
                ModelBone { name: "head".into(), parent: Some(0), length: 3.0 },
            ],
            parts,
            dropped_triangles: 0,
            missing_textures: vec![],
        }
    }

    fn image() -> Arc<RgbaImage> {
        Arc::new(RgbaImage { width: 1, height: 1, rgba: vec![255, 255, 255, 255] })
    }

    #[test]
    fn parts_are_merged_with_rebased_indices_and_one_bone_run_each() {
        let m = MergedMesh::build(&model(vec![part(0, &[[0, 1, 2]], 3), part(1, &[[0, 1, 2], [2, 1, 0]], 3)])).unwrap();
        assert_eq!(m.positions.len(), 6);
        assert_eq!(m.bone_runs, [BoneRun { bone: 0, start: 0, len: 3 }, BoneRun { bone: 1, start: 3, len: 3 }]);
        // The second part's triangles point at its own vertices (3..6), in the file's order.
        assert_eq!(m.indices, [0, 1, 2, 3, 4, 5, 5, 4, 3]);
        // Both parts are opaque and untextured: one batch for everything.
        assert_eq!(m.batches.len(), 1);
        assert_eq!((m.batches[0].kind, m.batches[0].first_index, m.batches[0].index_count), (BatchKind::Opaque, 0, 9));
        assert_eq!(m.triangle_count(), 3);
        assert!(m.overlay.iter().all(|&o| o == 0.0));
        assert!(format!("{m:?}").contains("triangles") && !format!("{m:?}").contains("200"));
    }

    #[test]
    fn textured_parts_batch_by_texture_and_come_after_the_plain_ones() {
        let (face, other) = (image(), image());
        let mut a = part(1, &[[0, 1, 2]], 3);
        a.texture = Some(Arc::clone(&face));
        let mut b = part(1, &[[0, 1, 2]], 3);
        b.texture = Some(Arc::clone(&face));
        let mut c = part(1, &[[0, 1, 2]], 3);
        c.texture = Some(Arc::clone(&other));
        // The textured parts come first in the file; the plain one last.
        let m = MergedMesh::build(&model(vec![a, b, c, part(0, &[[0, 1, 2]], 3)])).unwrap();
        let kinds: Vec<_> = m.batches.iter().map(|b| (b.kind, b.texture.is_some(), b.index_count)).collect();
        assert_eq!(
            kinds,
            [(BatchKind::Opaque, false, 3), (BatchKind::Opaque, true, 6), (BatchKind::Opaque, true, 3)],
            "plain first; the two parts sharing a texture are one batch"
        );
        // Batches tile the index buffer exactly, in order.
        let mut next = 0;
        for b in &m.batches {
            assert_eq!(b.first_index, next);
            next += b.index_count;
        }
        assert_eq!(next as usize, m.indices.len());
        // Textured vertices are flagged as overlays, the plain ones are not.
        assert_eq!(m.overlay.iter().filter(|&&o| o == 1.0).count(), 9);
        assert_eq!(m.overlay.iter().filter(|&&o| o == 0.0).count(), 3);
    }

    #[test]
    fn semi_transparent_parts_are_drawn_last_with_their_alpha_in_the_vertex_colour() {
        let mut glow = part(0, &[[0, 1, 2]], 3);
        glow.blend = PartBlend::Additive;
        glow.alpha = 0.5;
        let mut veil = part(0, &[[0, 1, 2]], 3);
        veil.blend = PartBlend::Alpha;
        veil.alpha = 0.25;
        let m = MergedMesh::build(&model(vec![glow, veil, part(1, &[[0, 1, 2]], 3)])).unwrap();
        let kinds: Vec<_> = m.batches.iter().map(|b| b.kind).collect();
        assert_eq!(kinds, [BatchKind::Opaque, BatchKind::Additive, BatchKind::Alpha]);
        assert_eq!(m.colors[0][3], 128, "the glow's vertices carry its alpha");
        assert_eq!(m.colors[3][3], 64);
        assert_eq!(m.colors[6][3], 255, "opaque parts stay opaque");
        assert_eq!(m.colors[0][..3], [200, 100, 50]);
        assert!(m.overlay.iter().all(|&o| o == 0.0), "blended parts are not depth-biased");
    }

    #[test]
    fn triangles_with_a_bad_index_are_left_out_and_empty_or_huge_models_are_refused() {
        // One good triangle, one pointing past the part's three vertices.
        let m = MergedMesh::build(&model(vec![part(0, &[[0, 1, 2], [0, 1, 3]], 3)])).unwrap();
        assert_eq!(m.indices, [0, 1, 2]);
        assert_eq!(MergedMesh::build(&model(vec![])).unwrap_err(), MeshError::Empty);
        assert_eq!(MergedMesh::build(&model(vec![part(0, &[[0, 1, 9]], 3)])).unwrap_err(), MeshError::Empty);
        let huge = model(vec![part(0, &[[0, 1, 2]], MAX_MODEL_VERTICES + 1)]);
        assert!(matches!(MergedMesh::build(&huge), Err(MeshError::TooManyVertices(n)) if n == MAX_MODEL_VERTICES + 1));
    }

    #[test]
    fn a_part_whose_texture_could_not_be_read_draws_untextured() {
        let mut p = part(0, &[[0, 1, 2]], 3);
        p.texture_name = Some("face".into());
        p.texture = None;
        let m = MergedMesh::build(&model(vec![p])).unwrap();
        assert_eq!((m.batches.len(), m.batches[0].texture.is_none()), (1, true));
    }

    /// Hair keeps its colour under a player's identity colour: the vertices of the HEAD bone (bone 1 here) whose colour is a
    /// hair colour are flagged `own`; the same colours on any other bone (the shoes), the head's cap and hood, decals and
    /// blended parts are not.
    #[test]
    fn hair_is_flagged_own_and_only_on_the_head_bone() {
        let coloured = |bone: usize, rgb: [u8; 3]| {
            let mut p = part(bone, &[[0, 1, 2]], 3);
            p.colors = vec![[rgb[0], rgb[1], rgb[2], 255]; 3];
            p
        };
        let hair = [46u8, 23, 0];
        let mut decal = coloured(1, hair);
        decal.texture = Some(image());
        let mut veil = coloured(1, hair);
        veil.blend = PartBlend::Alpha;
        veil.alpha = 0.5;
        // order: hair on the head, a white cap on the head, a blue hood on the head, shoes of the hair's colour on the hip,
        // a decal on the head, a veil on the head
        let m = MergedMesh::build(&model(vec![coloured(1, hair), coloured(1, [205, 211, 211]), coloured(1, [27, 62, 159]), coloured(0, hair), decal, veil])).unwrap();
        let per_part: Vec<f32> = m.own.chunks(3).map(|c| c[0]).collect();
        assert_eq!(m.own.len(), m.positions.len(), "one flag per vertex");
        assert_eq!(per_part, [1.0, 0.0, 0.0, 0.0, 0.0, 0.0], "only the hair: {per_part:?}");
        assert!(m.own.chunks(3).all(|c| c.iter().all(|&o| o == c[0])), "a part's vertices agree");
        // The decal keeps its colours through `overlay`, not through `own`.
        assert_eq!(m.overlay.chunks(3).map(|c| c[0]).collect::<Vec<_>>(), [0.0, 0.0, 0.0, 0.0, 1.0, 0.0]);
        // A skeleton with no bone called head flags nothing, and the name is matched without regard to case.
        let mut no_head = model(vec![coloured(1, hair)]);
        no_head.bones[1].name = "skull".into();
        assert!(MergedMesh::build(&no_head).unwrap().own.iter().all(|&o| o == 0.0));
        let mut loud = model(vec![coloured(1, hair)]);
        loud.bones[1].name = "HEAD".into();
        assert!(MergedMesh::build(&loud).unwrap().own.iter().all(|&o| o == 1.0));
    }

    /// A textured part with these texture coordinates, as the merged mesh keeps them.
    fn merged_uvs(uvs: &[[f32; 2]]) -> Vec<[f32; 2]> {
        let mut p = part(1, &[[0, 1, 2]], uvs.len().max(3));
        p.uvs = uvs.to_vec();
        p.texture = Some(image());
        MergedMesh::build(&model(vec![p])).unwrap().uvs
    }

    fn close(a: &[[f32; 2]], b: &[[f32; 2]]) -> bool {
        a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x[0] - y[0]).abs() < 1e-5 && (x[1] - y[1]).abs() < 1e-5)
    }

    /// The faces of the field models: the right eye is the left eye's coordinates plus 1 in `u` and a mouth has 5 added to
    /// `v` (the numbers of Cloud's own face, to the hundredth), and a clamping sampler would read nothing past 1: the whole
    /// number is taken off so that the second eye and the mouth are drawn.
    #[test]
    fn the_whole_number_shifts_of_a_faces_decals_are_taken_off() {
        // Cloud's right eye (u 1.00 to 1.97, v 0.03 to 0.94) and the left eye it mirrors (u 0.03 to 0.97).
        let right = merged_uvs(&[[1.00, 0.06], [1.84, 0.03], [1.03, 0.94]]);
        assert!(close(&right, &[[0.00, 0.06], [0.84, 0.03], [0.03, 0.94]]), "{right:?}");
        let left = merged_uvs(&[[0.03, 0.06], [0.81, 0.03], [0.97, 0.94]]);
        assert!(close(&left, &[[0.03, 0.06], [0.81, 0.03], [0.97, 0.94]]), "a part in cell 0 is untouched: {left:?}");
        // The mouth: u 0.03 to 0.97, v 5.00 to 5.94.
        let mouth = merged_uvs(&[[0.97, 5.22], [0.47, 5.00], [0.47, 5.94]]);
        assert!(close(&mouth, &[[0.97, 0.22], [0.47, 0.00], [0.47, 0.94]]), "{mouth:?}");
        // Both axes at once, and a negative cell.
        let both = merged_uvs(&[[2.1, 3.2], [2.9, 3.8], [2.5, 3.5]]);
        assert!(close(&both, &[[0.1, 0.2], [0.9, 0.8], [0.5, 0.5]]), "{both:?}");
        let below = merged_uvs(&[[-0.9, 0.1], [-0.2, 0.2], [-0.5, 0.9]]);
        assert!(close(&below, &[[0.1, 0.1], [0.8, 0.2], [0.5, 0.9]]), "{below:?}");
    }

    /// The file's own rounding: a coordinate that is a hair short of the cell's start is still in that cell, and a part
    /// that really spreads over two cells is left exactly as it was.
    #[test]
    fn a_part_is_only_moved_when_all_of_it_is_in_one_cell() {
        let hair_short = merged_uvs(&[[0.995, 0.1], [1.9, 0.2], [1.5, 0.3]]);
        assert!(close(&hair_short, &[[-0.005, 0.1], [0.9, 0.2], [0.5, 0.3]]), "{hair_short:?}");
        for spread in [
            vec![[0.5, 0.1], [1.5, 0.2], [1.0, 0.3]],
            vec![[0.9, 0.1], [1.3, 0.2], [1.1, 0.3]],
            vec![[0.1, 0.5], [0.2, 2.5], [0.3, 1.0]],
            vec![[1.0, 0.1], [2.5, 0.2], [1.5, 0.3]],
        ] {
            assert_eq!(merged_uvs(&spread), spread, "{spread:?} tiles: drawn as it always was");
        }
        // Ordinary and degenerate coordinates are never touched.
        for plain in [
            vec![[0.0, 0.0], [1.0, 1.0], [0.5, 0.5]],
            vec![[0.0, 0.0], [0.0, 0.0], [0.0, 0.0]],
            vec![[0.2, 0.2], [0.3, 0.4], [0.9, 0.8]],
            vec![[f32::NAN, 0.5], [0.5, f32::INFINITY], [0.5, 0.5]],
        ] {
            let got = merged_uvs(&plain);
            for (g, w) in got.iter().zip(&plain) {
                assert!((g[0] == w[0] || (g[0].is_nan() && w[0].is_nan())) && (g[1] == w[1] || g[1] == f32::INFINITY), "{plain:?} -> {got:?}");
            }
        }
        // An untextured part's coordinates mean nothing and are never shifted.
        let mut p = part(0, &[[0, 1, 2]], 3);
        p.uvs = vec![[1.2, 5.1], [1.8, 5.9], [1.5, 5.5]];
        assert_eq!(MergedMesh::build(&model(vec![p])).unwrap().uvs, [[1.2, 5.1], [1.8, 5.9], [1.5, 5.5]]);
    }
}
