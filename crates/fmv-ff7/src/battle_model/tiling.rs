//! Texture tiling baked into geometry.
//!
//! **Why.** 890 of the 1 198 textured groups of the owned battle locations (and three groups of
//! other battle models) have texture coordinates outside `0..1`, down to -16 and up to 16: the
//! texture is meant to repeat across the surface (a floor of 16 x 16 tiles of one 256 x 256
//! picture), as the game's own renderer draws it with repeat addressing. [`MeshPart::uvs`] are
//! documented as `0..1` across the texture (the field models' decals), and the client's avatar
//! sampler clamps to the edge on purpose (so that an eye at the edge of its texture does not blend
//! with the opposite edge), so a part with coordinates beyond `0..1` would draw as streaks of the
//! texture's edge row. The loader therefore rewrites such a group: each triangle is cut along the
//! integer lines of the texture plane (Sutherland-Hodgman against the cell `[k, k+1] x [m, m+1]`,
//! every attribute interpolated), and each piece's coordinates are moved back into `0..1`, so every
//! vertex of the result samples inside the texture and the existing drawing path shows the repeat.
//!
//! **Bounds.** A triangle that would be cut into more than [`MAX_CELLS_PER_TRIANGLE`] pieces, or a
//! group that would grow past [`MAX_TILED_TRIANGLES`] triangles, is not cut (its coordinates are
//! clamped as they stand), so hostile coordinates cost a wrong picture and never an allocation.

use crate::pmodel::{PCorner, PModel};

/// One corner of a triangle with everything that is interpolated across it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Corner {
    pub position: [f32; 3],
    /// RGBA, `0..255` as floats (so that cutting interpolates without rounding).
    pub color: [f32; 4],
    pub uv: [f32; 2],
}

/// Tolerance of the cell boundaries: a coordinate within this of an integer is on it.
pub(crate) const UV_EPSILON: f32 = 1.0e-4;
/// Most pieces one triangle may be cut into (a floor of 16 x 16 tiles is 256 cells).
pub(crate) const MAX_CELLS_PER_TRIANGLE: i128 = 4096;
/// Most triangles a tiled group may have: the client's merged mesh takes 200 000 vertices.
pub(crate) const MAX_TILED_TRIANGLES: usize = 60_000;
/// Pieces smaller than this in texture-plane area (cells) are slivers and are dropped.
const MIN_PIECE_AREA: f32 = 1.0e-9;

/// Whether `uv` lies outside the texture (beyond [`UV_EPSILON`]).
pub(crate) fn outside_unit_square(uv: [f32; 2]) -> bool {
    uv.iter().any(|&c| c < -UV_EPSILON || c > 1.0 + UV_EPSILON)
}

fn lerp(a: &Corner, b: &Corner, t: f32) -> Corner {
    let mix = |x: f32, y: f32| x + (y - x) * t;
    Corner {
        position: [mix(a.position[0], b.position[0]), mix(a.position[1], b.position[1]), mix(a.position[2], b.position[2])],
        color: [mix(a.color[0], b.color[0]), mix(a.color[1], b.color[1]), mix(a.color[2], b.color[2]), mix(a.color[3], b.color[3])],
        uv: [mix(a.uv[0], b.uv[0]), mix(a.uv[1], b.uv[1])],
    }
}

/// The part of convex `poly` on one side of `uv[axis] = bound` (the side with larger values when
/// `keep_greater`), by Sutherland-Hodgman; the new corners sit exactly on the bound.
fn clip(poly: &[Corner], axis: usize, bound: f32, keep_greater: bool) -> Vec<Corner> {
    let inside = |c: &Corner| {
        let v = c.uv.get(axis).copied().unwrap_or(0.0);
        if keep_greater {
            v >= bound
        } else {
            v <= bound
        }
    };
    let mut out = Vec::with_capacity(poly.len() + 2);
    for (i, a) in poly.iter().enumerate() {
        let Some(b) = poly.get((i + 1) % poly.len()) else { continue };
        let (a_in, b_in) = (inside(a), inside(b));
        if a_in {
            out.push(*a);
        }
        if a_in != b_in {
            let (av, bv) = (a.uv.get(axis).copied().unwrap_or(0.0), b.uv.get(axis).copied().unwrap_or(0.0));
            let t = if (bv - av).abs() > f32::EPSILON { ((bound - av) / (bv - av)).clamp(0.0, 1.0) } else { 0.0 };
            let mut c = lerp(a, b, t);
            if let Some(slot) = c.uv.get_mut(axis) {
                *slot = bound;
            }
            out.push(c);
        }
    }
    out
}

/// Twice the signed area of `poly` in the texture plane.
fn uv_area2(poly: &[Corner]) -> f32 {
    let mut sum = 0.0;
    for (i, a) in poly.iter().enumerate() {
        if let Some(b) = poly.get((i + 1) % poly.len()) {
            sum += a.uv[0] * b.uv[1] - b.uv[0] * a.uv[1];
        }
    }
    sum
}

/// First and last cell (cell `k` is `[k, k + 1]`) a coordinate range touches.
fn cell_range(lo: f32, hi: f32) -> (i64, i64) {
    let first = (lo + UV_EPSILON).floor() as i64;
    let last = ((hi - UV_EPSILON).ceil() as i64).saturating_sub(1).max(first);
    (first, last)
}

/// `corner` with its coordinates moved by `(-du, -dv)` and clamped into the texture.
fn moved(mut corner: Corner, du: i64, dv: i64) -> Corner {
    corner.uv = [(corner.uv[0] - du as f32).clamp(0.0, 1.0), (corner.uv[1] - dv as f32).clamp(0.0, 1.0)];
    corner
}

/// Cut `triangle` along the integer lines of the texture plane and push the pieces (triangles, in
/// the corner order of the input) to `out`, each with its coordinates inside `0..1`.
pub(crate) fn tile_triangle(triangle: [Corner; 3], out: &mut Vec<[Corner; 3]>) {
    let us = triangle.map(|c| c.uv[0]);
    let vs = triangle.map(|c| c.uv[1]);
    let fold = |a: [f32; 3]| a.iter().fold((f32::MAX, f32::MIN), |(lo, hi), &x| (lo.min(x), hi.max(x)));
    let ((umin, umax), (vmin, vmax)) = (fold(us), fold(vs));
    if !(umin.is_finite() && umax.is_finite() && vmin.is_finite() && vmax.is_finite()) {
        out.push(triangle.map(|c| moved(c, 0, 0)));
        return;
    }
    let ((u0, u1), (v0, v1)) = (cell_range(umin, umax), cell_range(vmin, vmax));
    let cells = (i128::from(u1) - i128::from(u0) + 1) * (i128::from(v1) - i128::from(v0) + 1);
    if cells > MAX_CELLS_PER_TRIANGLE {
        out.push(triangle.map(|c| moved(c, 0, 0)));
        return;
    }
    if cells == 1 {
        out.push(triangle.map(|c| moved(c, u0, v0)));
        return;
    }
    for cv in v0..=v1 {
        for cu in u0..=u1 {
            let mut poly = triangle.to_vec();
            poly = clip(&poly, 0, cu as f32, true);
            poly = clip(&poly, 0, cu as f32 + 1.0, false);
            poly = clip(&poly, 1, cv as f32, true);
            poly = clip(&poly, 1, cv as f32 + 1.0, false);
            if poly.len() < 3 || uv_area2(&poly).abs() < 2.0 * MIN_PIECE_AREA {
                continue;
            }
            // a fan from the first corner: (0, 1, 2), (0, 2, 3), ...
            let Some(first) = poly.first() else { continue };
            for pair in poly.get(1..).unwrap_or(&[]).windows(2) {
                if let [b, c] = pair {
                    out.push([moved(*first, cu, cv), moved(*b, cu, cv), moved(*c, cu, cv)]);
                }
            }
        }
    }
}

/// The triangles of a textured group with the texture's repeat baked in, in the output winding
/// (corners 1 and 2 of the file's swapped, like the untiled path): `None` when no coordinate of the
/// group leaves the texture (nothing to do) or when tiling would pass [`MAX_TILED_TRIANGLES`] (the
/// group is then drawn untiled).
pub(crate) fn tile_group(pmodel: &PModel, triangles: &[[PCorner; 3]]) -> Option<Vec<[Corner; 3]>> {
    let uv_of = |c: &PCorner| c.texcoord.and_then(|t| pmodel.texcoords.get(t)).copied().unwrap_or([0.0; 2]);
    if !triangles.iter().flatten().any(|c| outside_unit_square(uv_of(c))) {
        return None;
    }
    let corner = |c: &PCorner| Corner {
        position: pmodel.vertices.get(c.vertex).copied().unwrap_or([0.0; 3]),
        color: pmodel.vertex_colors.get(c.vertex).copied().unwrap_or([255; 4]).map(f32::from),
        uv: uv_of(c),
    };
    let mut pieces = Vec::new();
    for t in triangles {
        // The file's winding is clockwise seen from outside: swap corners 1 and 2.
        tile_triangle([corner(&t[0]), corner(&t[2]), corner(&t[1])], &mut pieces);
        if pieces.len() > MAX_TILED_TRIANGLES {
            return None;
        }
    }
    Some(pieces)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corner(x: f32, y: f32, u: f32, v: f32) -> Corner {
        Corner { position: [x, y, 0.0], color: [x, y, 0.0, 255.0], uv: [u, v] }
    }

    fn area3(t: &[Corner; 3]) -> f32 {
        let (a, b, c) = (t[0].position, t[1].position, t[2].position);
        ((b[0] - a[0]) * (c[1] - a[1]) - (c[0] - a[0]) * (b[1] - a[1])) / 2.0
    }

    #[test]
    fn a_triangle_inside_the_texture_is_left_alone() {
        let t = [corner(0.0, 0.0, 0.0, 0.0), corner(10.0, 0.0, 1.0, 0.0), corner(0.0, 10.0, 0.0, 1.0)];
        let mut out = Vec::new();
        tile_triangle(t, &mut out);
        assert_eq!(out, vec![t]);
        assert!(!outside_unit_square([0.0, 1.0]) && !outside_unit_square([1.00005, -0.00005]));
        assert!(outside_unit_square([1.01, 0.5]) && outside_unit_square([0.5, -0.01]));
    }

    #[test]
    fn a_triangle_inside_one_other_cell_is_moved_not_cut() {
        let t = [corner(0.0, 0.0, 3.25, -2.75), corner(10.0, 0.0, 3.75, -2.75), corner(0.0, 10.0, 3.25, -2.25)];
        let mut out = Vec::new();
        tile_triangle(t, &mut out);
        assert_eq!(out.len(), 1);
        for (got, want) in out[0].iter().zip(&t) {
            assert_eq!(got.position, want.position);
            assert!((got.uv[0] - (want.uv[0] - 3.0)).abs() < 1e-6 && (got.uv[1] - (want.uv[1] + 3.0)).abs() < 1e-6);
        }
    }

    #[test]
    fn a_quad_that_repeats_the_texture_is_cut_into_cells_that_keep_its_area_and_mapping() {
        // a 30 x 20 quad with the texture repeating 3 x 2 times, wound counter-clockwise
        let q = [corner(0.0, 0.0, 0.0, 0.0), corner(30.0, 0.0, 3.0, 0.0), corner(30.0, 20.0, 3.0, 2.0), corner(0.0, 20.0, 0.0, 2.0)];
        let mut out = Vec::new();
        tile_triangle([q[0], q[1], q[2]], &mut out);
        tile_triangle([q[0], q[2], q[3]], &mut out);
        assert!(out.len() > 2);
        let total: f32 = out.iter().map(area3).sum();
        assert!((total - 600.0).abs() < 0.05, "the pieces tile the quad: {total}");
        for piece in &out {
            assert!(area3(piece) > 0.0, "the winding is kept");
            for c in piece {
                assert!(c.uv.iter().all(|&x| (0.0..=1.0).contains(&x)), "{:?}", c.uv);
                // the sampled texel is where the original coordinates put it: the original u is
                // 3x/30 and v is 2y/20, so they differ from the moved ones by whole cells
                let (du, dv) = (c.position[0] / 10.0 - c.uv[0], c.position[1] / 10.0 - c.uv[1]);
                assert!((du - du.round()).abs() < 1e-3 && (dv - dv.round()).abs() < 1e-3, "{c:?}");
                // colours are interpolated with the position (the test colours are the position)
                assert!((c.color[0] - c.position[0]).abs() < 1e-3 && (c.color[1] - c.position[1]).abs() < 1e-3);
            }
        }
        // a quad piece is two triangles, so a few per cell: 6 cells
        assert!(out.len() <= 24, "{}", out.len());
    }

    #[test]
    fn a_tilted_triangle_across_many_cells_is_covered_exactly() {
        let t = [corner(0.0, 0.0, -2.3, 0.4), corner(50.0, 10.0, 4.8, 1.7), corner(5.0, 40.0, 0.2, 5.1)];
        let mut out = Vec::new();
        tile_triangle(t, &mut out);
        let total: f32 = out.iter().map(area3).sum();
        assert!((total - area3(&t)).abs() < 0.1, "{total} against {}", area3(&t));
        assert!(out.iter().flatten().all(|c| c.uv.iter().all(|&x| (0.0..=1.0).contains(&x))));
        assert!(out.len() > 20);
    }

    #[test]
    fn hostile_coordinates_are_bounded_and_never_panic() {
        let mut out = Vec::new();
        // far too many cells: not cut, clamped
        tile_triangle([corner(0.0, 0.0, -1e9, 0.0), corner(1.0, 0.0, 1e9, 0.0), corner(0.0, 1.0, 0.0, 1e9)], &mut out);
        assert_eq!(out.len(), 1);
        assert!(out[0].iter().all(|c| c.uv.iter().all(|&x| (0.0..=1.0).contains(&x))));
        // non-finite coordinates
        let mut out = Vec::new();
        tile_triangle([corner(0.0, 0.0, f32::NAN, 0.0), corner(1.0, 0.0, f32::INFINITY, 0.0), corner(0.0, 1.0, 0.0, f32::NEG_INFINITY)], &mut out);
        assert_eq!(out.len(), 1);
        // degenerate (zero-area, collinear) triangles do not loop or panic
        let mut out = Vec::new();
        tile_triangle([corner(0.0, 0.0, 0.0, 0.0), corner(1.0, 0.0, 2.0, 0.0), corner(2.0, 0.0, 4.0, 0.0)], &mut out);
        assert!(out.len() <= 8);
        // a triangle exactly on cell borders
        let mut out = Vec::new();
        tile_triangle([corner(0.0, 0.0, 1.0, 1.0), corner(1.0, 0.0, 2.0, 1.0), corner(0.0, 1.0, 1.0, 2.0)], &mut out);
        assert_eq!(out.len(), 1);
    }
}
