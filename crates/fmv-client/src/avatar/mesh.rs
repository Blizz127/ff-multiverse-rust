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

use fmv_ff7::{FieldModel, MeshPart, PartBlend, RgbaImage};
use std::sync::Arc;

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

impl MergedMesh {
    /// Merge the parts of `model`. Triangles with an out-of-range index are left out (the loader
    /// already drops them; this keeps the buffers sound whatever it is given). Parts that name a
    /// texture the loader could not read draw untextured.
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
            for i in 0..len {
                mesh.positions.push(part.positions.get(i).copied().unwrap_or([0.0; 3]));
                mesh.uvs.push(part.uvs.get(i).copied().unwrap_or([0.0; 2]));
                let [r, g, b, _] = part.colors.get(i).copied().unwrap_or([255; 4]);
                mesh.colors.push([r, g, b, alpha]);
                mesh.overlay.push(overlay);
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
}
