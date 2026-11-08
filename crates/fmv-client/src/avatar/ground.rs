//! Flat marks on the ground under the avatars: a soft shadow, the player's tint ring, the white
//! ring of the local player and the gold ring of a party member.
//!
//! They are drawn by the avatar renderer just before the models, *under* them, so a character
//! standing on its ring covers the middle of it and the ring still reads as lying on the floor.
//! Each shape is a polygon (or band) of points that were mapped through the field camera one by
//! one, so a circle on the ground comes out as the right ellipse.
//!
//! A shape also has a depth, the camera depth of the feet it lies around, and is depth tested like
//! the character itself: a tile that hides the character hides its marks (a ring must not float
//! over the wall a character is hidden behind), and one that is farther than the feet does not
//! (a ring under a character standing in front of a wall is not covered by it). One depth for the
//! whole shape: the ring is about 30 units across, the tile depth steps are 4 units per id.

use crate::render::{color, Camera, MAX_VERTICES};
use bytemuck::{Pod, Zeroable};
use fmv_core::Pos;

/// Segments of a ground circle.
pub const CIRCLE_SEGMENTS: usize = 28;

/// One corner of a mark's triangle: the NDC position, the colour (straight alpha: the pipeline
/// blends by it) and the camera-space depth the depth test uses.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct GroundVertex {
    pub pos: [f32; 2],
    pub color: [f32; 4],
    pub depth: f32,
}

/// A filled disc (`inner` empty) or a closed band between two outlines, in view space.
#[derive(Debug, Clone, PartialEq)]
pub struct GroundShape {
    pub outer: Vec<[f32; 2]>,
    /// The band's inner outline, as long as `outer`; empty for a filled disc.
    pub inner: Vec<[f32; 2]>,
    pub rgb: [u8; 3],
    pub alpha: f32,
    /// Camera depth of the feet the shape lies around (the depth test's value for every vertex).
    pub depth: f32,
}

/// The outline of a circle of `radius` field units lying in the plane of `centre`, mapped to view
/// space by `to_view`. `None` if any point cannot be drawn (behind the camera, not finite).
pub fn circle(to_view: &dyn Fn(Pos) -> Option<Pos>, centre: Pos, radius: f32) -> Option<Vec<[f32; 2]>> {
    (0..CIRCLE_SEGMENTS)
        .map(|i| {
            let angle = i as f32 / CIRCLE_SEGMENTS as f32 * std::f32::consts::TAU;
            let p = to_view(Pos::new(centre.x + radius * angle.cos(), centre.y + radius * angle.sin(), centre.z))?;
            (p.x.is_finite() && p.y.is_finite()).then_some([p.x, p.y])
        })
        .collect()
}

impl GroundShape {
    pub fn disc(outline: Vec<[f32; 2]>, rgb: [u8; 3], alpha: f32, depth: f32) -> Self {
        Self { outer: outline, inner: Vec::new(), rgb, alpha, depth }
    }

    pub fn band(outer: Vec<[f32; 2]>, inner: Vec<[f32; 2]>, rgb: [u8; 3], alpha: f32, depth: f32) -> Self {
        Self { outer, inner, rgb, alpha, depth }
    }

    /// The shape as flat-colour triangles for a `vw` x `vh` window fitted by `cam` (the camera the
    /// scene itself is drawn with), appended to `out`. Shapes that would not fit the vertex budget
    /// are skipped, never cut in half.
    pub fn push_vertices(&self, cam: &Camera, vw: u32, vh: u32, srgb: bool, out: &mut Vec<GroundVertex>) {
        let (hw, hh) = (vw.max(1) as f32 * 0.5, vh.max(1) as f32 * 0.5);
        let ndc = |p: [f32; 2]| {
            let q = cam.project(p[0], p[1]);
            [q[0] / hw, q[1] / hh]
        };
        let mut rgba = color(self.rgb, srgb);
        rgba[3] = self.alpha.clamp(0.0, 1.0);
        let n = self.outer.len();
        let band = !self.inner.is_empty();
        if n < 3 || (band && self.inner.len() != n) {
            return;
        }
        let triangles = if band { n * 2 } else { n };
        if out.len() + triangles * 3 > MAX_VERTICES {
            return;
        }
        let mut tri = |a: [f32; 2], b: [f32; 2], c: [f32; 2]| {
            let pts = [ndc(a), ndc(b), ndc(c)];
            if pts.iter().all(|p| p[0].is_finite() && p[1].is_finite()) {
                out.extend(pts.iter().map(|&pos| GroundVertex { pos, color: rgba, depth: self.depth }));
            }
        };
        if band {
            for i in 0..n {
                let j = (i + 1) % n;
                tri(self.outer[i], self.outer[j], self.inner[j]);
                tri(self.outer[i], self.inner[j], self.inner[i]);
            }
        } else {
            let centre = {
                let (sx, sy) = self.outer.iter().fold((0.0, 0.0), |(x, y), p| (x + p[0], y + p[1]));
                [sx / n as f32, sy / n as f32]
            };
            for i in 0..n {
                tri(centre, self.outer[i], self.outer[(i + 1) % n]);
            }
        }
    }
}

/// All shapes as one vertex list, in order (so later shapes are drawn over earlier ones).
pub fn vertices(shapes: &[GroundShape], cam: &Camera, vw: u32, vh: u32, srgb: bool) -> Vec<GroundVertex> {
    let mut out = Vec::new();
    for shape in shapes {
        shape.push_vertices(cam, vw, vh, srgb, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmv_adapter_api::Bounds;

    fn cam() -> Camera {
        // 200 x 100 window, view space -100..100 x -50..50, no margin effect worth caring about.
        let bounds = Bounds { min: Pos::new(-100.0, -50.0, 0.0), max: Pos::new(100.0, 50.0, 0.0) };
        Camera::fit(&bounds, 200.0, 100.0, 0.0)
    }

    fn identity(p: Pos) -> Option<Pos> {
        Some(Pos::new(p.x, p.y, 0.0))
    }

    #[test]
    fn a_ground_circle_is_a_ring_of_points_around_the_centre_mapped_through_the_view() {
        let pts = circle(&identity, Pos::new(10.0, -5.0, 3.0), 20.0).unwrap();
        assert_eq!(pts.len(), CIRCLE_SEGMENTS);
        for p in &pts {
            let d = (p[0] - 10.0).hypot(p[1] + 5.0);
            assert!((d - 20.0).abs() < 1e-3, "{d}");
        }
        // The view decides the shape: a view squashed to half height gives an ellipse.
        let squash = |p: Pos| Some(Pos::new(p.x, p.y * 0.5, 0.0));
        let e = circle(&squash, Pos::new(0.0, 0.0, 0.0), 20.0).unwrap();
        let (w, h) = (e.iter().map(|p| p[0]).fold(0.0f32, f32::max), e.iter().map(|p| p[1]).fold(0.0f32, f32::max));
        assert!((w - 20.0).abs() < 0.01 && (h - 10.0).abs() < 0.01, "{w} {h}");
        // One point that cannot be drawn spoils the whole circle (a half ring would mislead).
        let broken = |p: Pos| (p.x < 15.0).then(|| Pos::new(p.x, p.y, 0.0));
        assert_eq!(circle(&broken, Pos::new(0.0, 0.0, 0.0), 20.0), None);
        let nan = |_: Pos| Some(Pos::new(f32::NAN, 0.0, 0.0));
        assert_eq!(circle(&nan, Pos::new(0.0, 0.0, 0.0), 20.0), None);
    }

    #[test]
    fn discs_and_bands_become_triangles_in_ndc_with_their_colour_and_alpha() {
        let ring = circle(&identity, Pos::default(), 30.0).unwrap();
        let inner = circle(&identity, Pos::default(), 20.0).unwrap();
        let disc = GroundShape::disc(ring.clone(), [10, 20, 30], 0.5, 700.0);
        let band = GroundShape::band(ring, inner, [200, 100, 50], 1.0, 900.0);
        let v = vertices(&[disc, band], &cam(), 200, 100, false);
        assert_eq!(v.len(), (CIRCLE_SEGMENTS + CIRCLE_SEGMENTS * 2) * 3);
        assert!(v.iter().all(|v| v.pos.iter().all(|c| c.is_finite() && c.abs() <= 1.0)));
        // The disc is first and translucent, the band after it opaque.
        assert_eq!(v[0].color, [10.0 / 255.0, 20.0 / 255.0, 30.0 / 255.0, 0.5]);
        assert_eq!(v[v.len() - 1].color[3], 1.0);
        // Every vertex of a shape carries the shape's own depth.
        assert!(v[..CIRCLE_SEGMENTS * 3].iter().all(|x| x.depth == 700.0));
        assert!(v[CIRCLE_SEGMENTS * 3..].iter().all(|x| x.depth == 900.0));
        // The disc is centred on the window centre: its fan's first vertex is the centroid.
        assert!(v[0].pos[0].abs() < 1e-5 && v[0].pos[1].abs() < 1e-5, "{:?}", v[0].pos);
        // sRGB targets get linearised colours, like every other flat shape.
        let lin = vertices(&[GroundShape::disc(circle(&identity, Pos::default(), 5.0).unwrap(), [128, 128, 128], 1.0, 1.0)], &cam(), 200, 100, true);
        assert!(lin[0].color[0] < 0.25 && lin[0].color[0] > 0.2);
    }

    #[test]
    fn malformed_shapes_draw_nothing_and_the_budget_is_respected() {
        let ring = circle(&identity, Pos::default(), 30.0).unwrap();
        // Too few points; an inner outline of the wrong length.
        assert!(vertices(&[GroundShape::disc(vec![[0.0, 0.0], [1.0, 1.0]], [1, 2, 3], 1.0, 1.0)], &cam(), 200, 100, false).is_empty());
        assert!(vertices(&[GroundShape::band(ring.clone(), ring[..5].to_vec(), [1, 2, 3], 1.0, 1.0)], &cam(), 200, 100, false).is_empty());
        // Non-finite points are skipped triangle by triangle, never forwarded.
        let mut bad = ring.clone();
        bad[3] = [f32::NAN, 0.0];
        let v = vertices(&[GroundShape::disc(bad, [1, 2, 3], 1.0, 1.0)], &cam(), 200, 100, false);
        assert!(v.len() < CIRCLE_SEGMENTS * 3 && v.iter().all(|v| v.pos.iter().all(|c| c.is_finite())));
        // A zero-sized window does not divide by zero.
        assert!(vertices(&[GroundShape::disc(ring, [1, 2, 3], 1.0, 1.0)], &cam(), 0, 0, false).iter().all(|v| v.pos.iter().all(|c| c.is_finite())));
    }
}
