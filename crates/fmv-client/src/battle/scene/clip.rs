//! Cutting a mesh to the camera's near plane, so that nothing behind the camera reaches the GPU.
//!
//! The avatar renderer ([`crate::avatar::gpu`]) is built for models that stand in front of the
//! camera: its depth mapping clamps everything nearer than the near plane to depth 0 instead of
//! clipping against it, so a triangle with a vertex BEHIND the camera comes out wrong (the part that
//! is in front is lost or smeared). A battle location is a big model the camera stands inside of, so
//! its triangles must be cut before they are drawn. The camera does not move in a battle, so this is
//! done once per camera for the whole battle, not per frame: [`clip_part`] cuts one mesh part,
//! keeping every attribute (texture coordinates, colours, normals) interpolated along the cut.

// `chunks_exact(3)` over triangle indices would be `as_chunks` on Rust 1.88+; the workspace keeps its
// MSRV lower (see `avatar`), so the lint is allowed here as it is there.
#![allow(clippy::chunks_exact_to_as_chunks)]

use super::camera::V3;
use fmv_ff7::MeshPart;

/// A vertex of the part, as the cut needs it.
#[derive(Clone, Copy)]
struct Corner {
    position: [f32; 3],
    normal: [f32; 3],
    uv: [f32; 2],
    color: [u8; 4],
}

fn corner(part: &MeshPart, i: usize) -> Corner {
    Corner {
        position: part.positions.get(i).copied().unwrap_or([0.0; 3]),
        normal: part.normals.get(i).copied().unwrap_or([0.0; 3]),
        uv: part.uvs.get(i).copied().unwrap_or([0.0; 2]),
        color: part.colors.get(i).copied().unwrap_or([255; 4]),
    }
}

fn lerp(a: Corner, b: Corner, t: f32) -> Corner {
    let mix = |x: f32, y: f32| x + (y - x) * t;
    Corner {
        position: [mix(a.position[0], b.position[0]), mix(a.position[1], b.position[1]), mix(a.position[2], b.position[2])],
        normal: [mix(a.normal[0], b.normal[0]), mix(a.normal[1], b.normal[1]), mix(a.normal[2], b.normal[2])],
        uv: [mix(a.uv[0], b.uv[0]), mix(a.uv[1], b.uv[1])],
        color: [0, 1, 2, 3].map(|k| mix(f32::from(a.color[k]), f32::from(b.color[k])).round().clamp(0.0, 255.0) as u8),
    }
}

/// `part` cut to the half-space `forward . (p - eye) >= near` (positions are taken as they are, in
/// the space `eye` and `forward` are in). Triangles wholly in front are kept as they are, those
/// wholly behind are dropped, those across the plane become one or two. Only the vertices the
/// remaining triangles use are kept (so every vertex of the result is in front of the plane).
/// `None` when nothing is left.
pub fn clip_part(part: &MeshPart, eye: V3, forward: V3, near: f64) -> Option<MeshPart> {
    let n = part.positions.len();
    let distance = |p: [f32; 3]| -> f64 {
        forward[0] * (f64::from(p[0]) - eye[0]) + forward[1] * (f64::from(p[1]) - eye[1]) + forward[2] * (f64::from(p[2]) - eye[2]) - near
    };
    let d: Vec<f64> = part.positions.iter().map(|&p| distance(p)).collect();
    let mut out = MeshPart {
        positions: part.positions.clone(),
        normals: (0..n).map(|i| corner(part, i).normal).collect(),
        uvs: (0..n).map(|i| corner(part, i).uv).collect(),
        colors: (0..n).map(|i| corner(part, i).color).collect(),
        indices: Vec::with_capacity(part.indices.len()),
        ..part.clone()
    };
    for tri in part.indices.chunks_exact(3) {
        let ids = [tri[0] as usize, tri[1] as usize, tri[2] as usize];
        if ids.iter().any(|&i| i >= n) {
            continue;
        }
        let ds = ids.map(|i| d[i]);
        if ds.iter().all(|&v| v >= 0.0) {
            out.indices.extend_from_slice(tri);
            continue;
        }
        if ds.iter().all(|&v| v < 0.0) {
            continue;
        }
        // Across the plane: cut the polygon (Sutherland-Hodgman) and fan it.
        let mut polygon: Vec<u32> = Vec::with_capacity(4);
        for k in 0..3 {
            let (a, b) = (ids[k], ids[(k + 1) % 3]);
            let (da, db) = (ds[k], ds[(k + 1) % 3]);
            if da >= 0.0 {
                polygon.push(a as u32);
            }
            if (da >= 0.0) != (db >= 0.0) {
                let t = (da / (da - db)) as f32;
                let c = lerp(corner(part, a), corner(part, b), t);
                polygon.push(u32::try_from(out.positions.len()).ok()?);
                out.positions.push(c.position);
                out.normals.push(c.normal);
                out.uvs.push(c.uv);
                out.colors.push(c.color);
            }
        }
        for k in 1..polygon.len().saturating_sub(1) {
            out.indices.extend_from_slice(&[polygon[0], polygon[k], polygon[k + 1]]);
        }
    }
    if out.indices.is_empty() {
        return None;
    }
    // Keep only what the triangles use, in the order they first use it.
    let mut new_index: Vec<Option<u32>> = vec![None; out.positions.len()];
    let (mut positions, mut normals, mut uvs, mut colors) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for index in &mut out.indices {
        let old = *index as usize;
        let slot = &mut new_index[old];
        let mapped = *slot.get_or_insert_with(|| {
            positions.push(out.positions[old]);
            normals.push(out.normals[old]);
            uvs.push(out.uvs[old]);
            colors.push(out.colors[old]);
            (positions.len() - 1) as u32
        });
        *index = mapped;
    }
    (out.positions, out.normals, out.uvs, out.colors) = (positions, normals, uvs, colors);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmv_ff7::PartBlend;

    fn part(positions: &[[f32; 3]], colors: &[[u8; 4]], uvs: &[[f32; 2]], indices: &[u32]) -> MeshPart {
        MeshPart {
            bone_index: 0,
            positions: positions.to_vec(),
            normals: vec![[0.0, -1.0, 0.0]; positions.len()],
            uvs: uvs.to_vec(),
            colors: colors.to_vec(),
            indices: indices.to_vec(),
            texture: None,
            texture_name: Some("t".into()),
            blend: PartBlend::Opaque,
            alpha: 1.0,
        }
    }

    /// The camera at the origin looking along +z, near plane at z = 100.
    const EYE: V3 = [0.0; 3];
    const FORWARD: V3 = [0.0, 0.0, 1.0];

    fn area(part: &MeshPart) -> f32 {
        part.indices
            .chunks_exact(3)
            .map(|t| {
                let p = |i: u32| part.positions[i as usize];
                let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
                let (u, v) = ([b[0] - a[0], b[1] - a[1]], [c[0] - a[0], c[1] - a[1]]);
                (u[0] * v[1] - u[1] * v[0]).abs() * 0.5
            })
            .sum()
    }

    #[test]
    fn triangles_in_front_are_kept_exactly_and_behind_dropped() {
        let front = part(
            &[[0.0, 0.0, 200.0], [10.0, 0.0, 300.0], [0.0, 10.0, 400.0]],
            &[[1, 2, 3, 4]; 3],
            &[[0.0; 2]; 3],
            &[0, 1, 2],
        );
        let kept = clip_part(&front, EYE, FORWARD, 100.0).unwrap();
        assert_eq!(kept.indices, [0, 1, 2]);
        assert_eq!(kept.positions, front.positions);
        let behind = part(&[[0.0, 0.0, -5.0], [10.0, 0.0, 50.0], [0.0, 10.0, 99.0]], &[[0; 4]; 3], &[[0.0; 2]; 3], &[0, 1, 2]);
        assert!(clip_part(&behind, EYE, FORWARD, 100.0).is_none(), "nothing in front: nothing left");
    }

    #[test]
    fn a_triangle_across_the_plane_is_cut_on_it_with_its_attributes_interpolated() {
        // One corner in front (z = 300), two behind (z = 0): a triangle on the plane z = 100, 2/3 of the way.
        let p = part(
            &[[0.0, 0.0, 300.0], [60.0, 0.0, 0.0], [0.0, 60.0, 0.0]],
            &[[255, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255]],
            &[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
            &[0, 1, 2],
        );
        let cut = clip_part(&p, EYE, FORWARD, 100.0).unwrap();
        assert_eq!(cut.indices.len(), 3, "one triangle");
        assert!(cut.indices.iter().all(|&i| cut.positions[i as usize][2] >= 100.0 - 1e-3), "all in front of the plane");
        // The two new corners lie on z = 100, two thirds of the way from the front corner to each back one.
        let on_plane: Vec<usize> = cut.indices.iter().map(|&i| i as usize).filter(|&i| (cut.positions[i][2] - 100.0).abs() < 1e-3).collect();
        assert_eq!(on_plane.len(), 2);
        let toward_second = on_plane.iter().copied().find(|&i| cut.positions[i][0] > 1.0).unwrap();
        assert!((cut.positions[toward_second][0] - 40.0).abs() < 1e-3, "{:?}", cut.positions[toward_second]);
        // Colour and texture coordinates are the same blend: 1/3 of the way to the second corner's.
        let (color, uv) = (cut.colors[toward_second], cut.uvs[toward_second]);
        assert_eq!(color, [85, 170, 0, 255], "two thirds of the way from the front corner's red to the second corner's green");
        assert!((uv[0] - 2.0 / 3.0).abs() < 1e-4 && uv[1].abs() < 1e-4, "{uv:?}");
        // The part that is kept is the right size: the whole is 1/2 * 60 * 60 seen from above, and
        // the piece in front is a smaller triangle (the whole scaled by 2/3 about the front corner).
        assert!((area(&cut) - 0.5 * 40.0 * 40.0).abs() < 1.0, "{}", area(&cut));
        assert_eq!(cut.texture_name.as_deref(), Some("t"), "everything else of the part stays");
    }

    #[test]
    fn two_corners_in_front_make_a_quadrilateral_of_two_triangles() {
        let p = part(&[[0.0, 0.0, 300.0], [60.0, 0.0, 300.0], [0.0, 60.0, 0.0]], &[[9; 4]; 3], &[[0.0; 2]; 3], &[0, 1, 2]);
        let cut = clip_part(&p, EYE, FORWARD, 100.0).unwrap();
        assert_eq!(cut.indices.len(), 6);
        // The quadrilateral is the whole (1800) minus the small triangle behind the plane (200).
        assert!((area(&cut) - 1600.0).abs() < 1.0, "{}", area(&cut));
        assert_eq!(cut.positions.len(), 4, "the two corners in front and the two new ones; the one behind is gone");
    }

    #[test]
    fn the_plane_follows_the_camera_not_the_axes() {
        // A camera looking along -x from x = 1000: a triangle at x = 900 is in front, one at x = 1100 behind.
        let eye = [1000.0, 0.0, 0.0];
        let forward = [-1.0, 0.0, 0.0];
        let p = part(&[[900.0, 0.0, 0.0], [900.0, 5.0, 0.0], [900.0, 0.0, 5.0], [1100.0, 0.0, 0.0], [1100.0, 5.0, 0.0], [1100.0, 0.0, 5.0]], &[[0; 4]; 6], &[[0.0; 2]; 6], &[0, 1, 2, 3, 4, 5]);
        let cut = clip_part(&p, eye, forward, 10.0).unwrap();
        assert_eq!(cut.indices, [0, 1, 2], "only the one in front");
    }

    #[test]
    fn broken_input_is_left_out_not_trusted() {
        // An index past the vertices, a dangling index list, missing per-vertex arrays: no panic.
        let mut p = part(&[[0.0, 0.0, 200.0], [10.0, 0.0, 200.0], [0.0, 10.0, 200.0]], &[[1; 4]; 3], &[[0.0; 2]; 3], &[0, 1, 2, 0, 1, 7, 0, 1]);
        p.colors.clear();
        p.uvs.clear();
        let cut = clip_part(&p, EYE, FORWARD, 100.0).unwrap();
        assert_eq!(cut.indices, [0, 1, 2], "the triangle with an index past the end and the loose indices are dropped");
        assert_eq!((cut.colors.len(), cut.uvs.len(), cut.normals.len()), (3, 3, 3), "the arrays are made parallel");
        assert!(clip_part(&part(&[], &[], &[], &[]), EYE, FORWARD, 100.0).is_none());
    }
}
