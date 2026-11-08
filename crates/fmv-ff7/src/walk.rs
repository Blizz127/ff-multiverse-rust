//! Walkmesh navigation: point location, edge-access aware movement validation and spawn
//! selection over a decoded [`Walkmesh`].
//!
//! **UNVERIFIED AGAINST OWNED MEDIA.** Everything here rests on the walkmesh layout described
//! in [`crate::field`] (FFRTT "FF7/Field/Walkmesh"): three vertices per triangle and, per
//! triangle, three access words, where word `e` is "the triangle you should be in if you cross
//! edge `e`" (edge 0 = v0-v1, 1 = v1-v2, 2 = v2-v0) and `0xFFFF` means "this edge cannot be
//! crossed". The page does not say whether links are symmetric or whether indices are 0-based;
//! this module assumes 0-based and does not require symmetry (a link is followed in the
//! direction it is stored). x/y are the ground plane and z is height, as everywhere in this
//! crate (also unconfirmed).
//!
//! # Why not just "is the destination inside some triangle"
//!
//! A destination-only test lets a client climb through a blocked edge onto an adjacent
//! raised platform, step off a bridge onto the floor below, or hop between stacked layers
//! wherever they overlap in x/y: walls and level changes in FF7 are carried by the access
//! words, not by the geometry. [`Surface::route`] therefore asks whether the straight segment
//! `from -> to` can be walked **through the triangle graph**.
//!
//! # The routing rule
//!
//! Every triangle is widened by [`EDGE_TOLERANCE`] (so positions that sit exactly on an edge
//! are inside, and two neighbours overlap a little along their shared edge). For the segment
//! `p(t) = from + t (to - from)`, `t` in `[0, 1]`, each triangle then covers one closed
//! interval of `t` (found by clipping `t` against the three barycentric half-planes and the
//! bounding box; barycentric weights are affine in the position, so this is exact). With those
//! intervals:
//!
//! * **start**: triangles whose interval contains `t = 0`, restricted to the layer nearest the
//!   entity's current height (where several triangles overlap at `from`, the ones whose
//!   surface is within [`LAYER_TOLERANCE`] of the nearest one to `from.z`);
//! * **step**: from triangle `T` to a linked triangle `N` (any non-blocked access word of
//!   `T`) when the two intervals overlap, i.e. the segment really passes from one into the
//!   other at that point;
//! * **goal**: a reached triangle whose interval contains `t = 1`.
//!
//! The move is walkable iff a goal is reached. Because only linked triangles are followed and
//! only where the segment actually is, blocked edges, disconnected layers and holes all refuse
//! the move, while a segment that passes exactly through a vertex (or runs along an edge) is
//! handled without special cases, and a link between two triangles cannot be used to skip a
//! third triangle that the segment crosses in between (the intervals do not overlap). Among
//! several reachable goals the one whose surface is closest to the hint height wins, ties
//! going to the lowest triangle index.
//!
//! The step rule uses the *set* of a triangle's links, not which edge each word is stored
//! under, so a mistake in the (unverified) edge order cannot make legitimate crossings fail.
//! For a well-formed mesh the two readings agree (two triangles share at most one edge). Only
//! the wall handling below depends on the documented edge order, and an error there moves a
//! position by a few thousandths of a unit at most.
//!
//! # Positions on a wall
//!
//! A position exactly on a blocked edge belongs to both triangles, and a stateless validator
//! cannot tell which side the entity came from, so an entity could stop on the wall and then
//! step across. To close that, an accepted position closer than [`WALL_TRIGGER`] to a blocked
//! edge of its triangle is moved toward the triangle's centroid until it is [`WALL_MARGIN`]
//! away (a few thousandths of a walkmesh unit in practice, and never more than
//! [`MAX_NUDGE`]). The result is judged after rounding to `f32` (positions are stored as
//! `f32`), and if the rounded point would not clear the wall the position is left as it was.
//! Needle triangles thinner than the margin, and meshes with coordinates so large that `f32`
//! cannot resolve a few thousandths (beyond about 16 000 units), keep the closed boundary;
//! that is a known gap.
//!
//! # Other known gaps
//!
//! * A `from` that is not on the mesh (or not finite) has no starting triangle; callers fall
//!   back to a destination-only check for it (see `area`). The server never passes one.
//! * Heights are not otherwise constrained: any climb the links allow is allowed.
//! * Links are trusted as stored. [`Surface::link_report`] counts how many are symmetric and
//!   geometrically adjacent so the layout assumptions can be checked on real data.

use crate::field::{Vertex, Walkmesh};
use fmv_core::Pos;

/// A position this close (walkmesh units) to a triangle edge still counts as inside it, to
/// absorb `f32` rounding on positions that sit exactly on an edge.
pub(crate) const EDGE_TOLERANCE: f64 = 1e-3;
/// Accepted positions closer than this to a blocked edge are moved inward.
const WALL_TRIGGER: f64 = 1.5 * EDGE_TOLERANCE;
/// ... until they are this far from it.
const WALL_MARGIN: f64 = 2.5 * EDGE_TOLERANCE;
/// Largest distance (walkmesh units) a position may be moved to clear a wall.
const MAX_NUDGE: f64 = 0.05;
/// Two surfaces at one x/y closer than this in height count as the same layer.
const LAYER_TOLERANCE: f64 = 1e-3;

fn cross(ux: f64, uy: f64, vx: f64, vy: f64) -> f64 {
    ux * vy - uy * vx
}

/// Clamp negative (within-tolerance) weights to zero and renormalise to sum to 1.
fn normalize(raw: [f64; 3]) -> Option<[f64; 3]> {
    let [wa, wb, wc] = raw.map(|w| w.max(0.0));
    let sum = wa + wb + wc;
    if sum.is_nan() || sum <= 0.0 {
        return None;
    }
    Some([wa / sum, wb / sum, wc / sum])
}

/// Keep the part of `[lo, hi]` where `base + t * slope >= 0`.
fn clip(lo: &mut f64, hi: &mut f64, base: f64, slope: f64) {
    if slope > 0.0 {
        *lo = lo.max(-base / slope);
    } else if slope < 0.0 {
        *hi = hi.min(base / -slope);
    } else if base < 0.0 {
        *hi = f64::NEG_INFINITY; // constant and violated: empty
    }
}

/// A non-degenerate walkmesh triangle with everything the queries need precomputed.
#[derive(Debug, Clone)]
struct Tri {
    a: [f64; 3],
    b: [f64; 3],
    c: [f64; 3],
    /// Twice the signed area in the x/y plane (never zero).
    area2: f64,
    min_x: f64,
    max_x: f64,
    min_y: f64,
    max_y: f64,
    /// Distance from vertex `k` to the opposite edge. A barycentric weight times this height
    /// is the distance from the opposite edge's line.
    height: [f64; 3],
}

impl Tri {
    /// `None` for a degenerate (zero-area) triangle.
    fn new(tri: &[Vertex; 3]) -> Option<Self> {
        let p = |v: &Vertex| [f64::from(v.x), f64::from(v.y), f64::from(v.z)];
        let [va, vb, vc] = tri;
        let (a, b, c) = (p(va), p(vb), p(vc));
        let area2 = cross(b[0] - a[0], b[1] - a[1], c[0] - a[0], c[1] - a[1]);
        if area2 == 0.0 || !area2.is_finite() {
            return None;
        }
        let len = |p: [f64; 3], q: [f64; 3]| (p[0] - q[0]).hypot(p[1] - q[1]);
        let abs2 = area2.abs();
        Some(Self {
            a,
            b,
            c,
            area2,
            min_x: a[0].min(b[0]).min(c[0]),
            max_x: a[0].max(b[0]).max(c[0]),
            min_y: a[1].min(b[1]).min(c[1]),
            max_y: a[1].max(b[1]).max(c[1]),
            // vertex a faces edge bc, b faces ca, c faces ab
            height: [abs2 / len(b, c), abs2 / len(c, a), abs2 / len(a, b)],
        })
    }

    /// Barycentric weights of `(x, y)` (sum 1; negative outside), affine in the position.
    fn raw_weights(&self, x: f64, y: f64) -> [f64; 3] {
        let (a, b, c) = (&self.a, &self.b, &self.c);
        [
            cross(c[0] - b[0], c[1] - b[1], x - b[0], y - b[1]) / self.area2,
            cross(a[0] - c[0], a[1] - c[1], x - c[0], y - c[1]) / self.area2,
            cross(b[0] - a[0], b[1] - a[1], x - a[0], y - a[1]) / self.area2,
        ]
    }

    /// Distance of `(x, y)` from each edge's line in edge order (v0-v1, v1-v2, v2-v0),
    /// positive inside.
    fn edge_distances(&self, x: f64, y: f64) -> [f64; 3] {
        let (w, h) = (self.raw_weights(x, y), self.height);
        // edge v0-v1 faces vertex 2, v1-v2 faces vertex 0, v2-v0 faces vertex 1
        [w[2] * h[2], w[0] * h[0], w[1] * h[1]]
    }

    /// Normalised weights of `(x, y)` if it lies inside (within [`EDGE_TOLERANCE`] of every
    /// edge and of the bounding box).
    fn inside(&self, x: f64, y: f64) -> Option<[f64; 3]> {
        if x < self.min_x - EDGE_TOLERANCE
            || x > self.max_x + EDGE_TOLERANCE
            || y < self.min_y - EDGE_TOLERANCE
            || y > self.max_y + EDGE_TOLERANCE
        {
            return None;
        }
        let raw = self.raw_weights(x, y);
        if raw.iter().zip(&self.height).any(|(&w, &h)| w * h < -EDGE_TOLERANCE) {
            return None;
        }
        normalize(raw)
    }

    /// Interval of `t` in `[0, 1]` for which `p + t (q - p)` is inside (same definition as
    /// [`Tri::inside`]), or `None` if the segment misses the triangle.
    fn span(&self, p: (f64, f64), q: (f64, f64)) -> Option<(f64, f64)> {
        let (wp, wq) = (self.raw_weights(p.0, p.1), self.raw_weights(q.0, q.1));
        let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
        for ((&at_p, &at_q), &h) in wp.iter().zip(&wq).zip(&self.height) {
            clip(&mut lo, &mut hi, at_p + EDGE_TOLERANCE / h, at_q - at_p);
        }
        clip(&mut lo, &mut hi, p.0 - (self.min_x - EDGE_TOLERANCE), q.0 - p.0);
        clip(&mut lo, &mut hi, (self.max_x + EDGE_TOLERANCE) - p.0, p.0 - q.0);
        clip(&mut lo, &mut hi, p.1 - (self.min_y - EDGE_TOLERANCE), q.1 - p.1);
        clip(&mut lo, &mut hi, (self.max_y + EDGE_TOLERANCE) - p.1, p.1 - q.1);
        (lo <= hi).then_some((lo, hi))
    }

    fn z_at(&self, w: [f64; 3]) -> f64 {
        w[0] * self.a[2] + w[1] * self.b[2] + w[2] * self.c[2]
    }

    fn centroid(&self) -> [f64; 3] {
        [
            (self.a[0] + self.b[0] + self.c[0]) / 3.0,
            (self.a[1] + self.b[1] + self.c[1]) / 3.0,
            (self.a[2] + self.b[2] + self.c[2]) / 3.0,
        ]
    }
}

/// A triangle the segment being routed meets, with the part of the segment inside it.
#[derive(Debug, Clone, Copy)]
struct Touch {
    tri: usize,
    lo: f64,
    hi: f64,
}

/// Outcome of [`Surface::route`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Route {
    /// The segment can be walked. `(x, y)` is the accepted position (the destination, moved
    /// inward if it was on a wall) and `z` the surface height there.
    Reached { x: f64, y: f64, z: f64 },
    /// `from` is on the mesh but the destination cannot be reached from it. `z` is the
    /// surface height at `from` (on the layer nearest the hint).
    Blocked { z: f64 },
    /// `from` is not on the mesh, so there is nothing to route from.
    OffMesh,
}

/// How well the access words of a mesh match the assumptions in the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LinkReport {
    /// Links that point at a valid, different triangle.
    pub links: usize,
    /// Links whose target links back.
    pub symmetric: usize,
    /// Links whose target contains both end points of the linking edge (a real neighbour).
    pub adjacent: usize,
    /// Edges that are blocked (`0xFFFF`, or an index outside the mesh).
    pub blocked_edges: usize,
}

/// The walkable surface of one field: usable triangles plus the links between them.
#[derive(Debug, Clone)]
pub(crate) struct Surface {
    /// Aligned with the walkmesh's triangle list (`None` = degenerate). Access words refer to
    /// original triangle indices, so degenerate triangles keep their slot.
    tris: Vec<Option<Tri>>,
    /// Valid link targets per triangle edge (`None` = blocked).
    links: Vec<[Option<usize>; 3]>,
    /// The raw vertices, for [`Surface::link_report`].
    raw: Vec<[Vertex; 3]>,
}

fn find(parent: &mut [usize], mut i: usize) -> usize {
    loop {
        let p = parent.get(i).copied().unwrap_or(i);
        if p == i {
            return i;
        }
        let grand = parent.get(p).copied().unwrap_or(p);
        if let Some(slot) = parent.get_mut(i) {
            *slot = grand; // path halving
        }
        i = grand;
    }
}

fn union(parent: &mut [usize], a: usize, b: usize) {
    let (ra, rb) = (find(parent, a), find(parent, b));
    if ra != rb {
        if let Some(slot) = parent.get_mut(rb) {
            *slot = ra;
        }
    }
}

impl Surface {
    pub(crate) fn new(mesh: &Walkmesh) -> Self {
        let tris = mesh.triangles.iter().map(Tri::new).collect();
        let links = (0..mesh.len())
            .map(|i| [0usize, 1, 2].map(|e| mesh.neighbor(i, e).filter(|&j| j != i)))
            .collect();
        Self { tris, links, raw: mesh.triangles.clone() }
    }

    fn tri(&self, index: usize) -> Option<&Tri> {
        self.tris.get(index)?.as_ref()
    }

    fn usable(&self) -> impl Iterator<Item = (usize, &Tri)> {
        self.tris.iter().enumerate().filter_map(|(i, t)| t.as_ref().map(|t| (i, t)))
    }

    /// Number of non-degenerate triangles.
    pub(crate) fn usable_count(&self) -> usize {
        self.usable().count()
    }

    /// Surface height of triangle `index` at `(x, y)` if that point is inside it.
    fn height_in(&self, index: usize, p: (f64, f64)) -> Option<f64> {
        let t = self.tri(index)?;
        Some(t.z_at(t.inside(p.0, p.1)?))
    }

    /// The triangle containing `(x, y)` and the surface height there, preferring the surface
    /// closest to `hint` where several overlap (ties and the no-hint case keep the lowest
    /// triangle index). Ignores links: this is a pure point query.
    pub(crate) fn locate(&self, x: f64, y: f64, hint: Option<f64>) -> Option<(usize, f64)> {
        let mut best: Option<(f64, usize, f64)> = None; // (distance from hint, triangle, z)
        for (i, t) in self.usable() {
            if let Some(w) = t.inside(x, y) {
                let z = t.z_at(w);
                let score = hint.map_or(0.0, |h| (z - h).abs());
                if best.is_none_or(|(s, _, _)| score < s) {
                    best = Some((score, i, z));
                }
            }
        }
        best.map(|(_, i, z)| (i, z))
    }

    /// Triangles meeting the segment `p -> q`, ascending by index.
    fn touched(&self, p: (f64, f64), q: (f64, f64)) -> Vec<Touch> {
        let (x0, x1) = (p.0.min(q.0) - EDGE_TOLERANCE, p.0.max(q.0) + EDGE_TOLERANCE);
        let (y0, y1) = (p.1.min(q.1) - EDGE_TOLERANCE, p.1.max(q.1) + EDGE_TOLERANCE);
        self.usable()
            .filter(|(_, t)| {
                t.max_x + EDGE_TOLERANCE >= x0
                    && t.min_x - EDGE_TOLERANCE <= x1
                    && t.max_y + EDGE_TOLERANCE >= y0
                    && t.min_y - EDGE_TOLERANCE <= y1
            })
            .filter_map(|(tri, t)| t.span(p, q).map(|(lo, hi)| Touch { tri, lo, hi }))
            .collect()
    }

    /// Can the straight segment `from -> to` be walked through the triangle graph (see the
    /// module docs)? `hint_z` is the entity's current height (`None` if unknown: the lowest
    /// triangle under `from` decides the layer). Both points must be finite.
    pub(crate) fn route(&self, from: (f64, f64), hint_z: Option<f64>, to: (f64, f64)) -> Route {
        let touched = self.touched(from, to);

        // Triangles under `from`, with the surface height there.
        let under_from: Vec<(usize, f64)> = touched
            .iter()
            .enumerate()
            .filter(|(_, t)| t.lo <= 0.0)
            .filter_map(|(slot, t)| Some((slot, self.height_in(t.tri, from)?)))
            .collect();
        let Some(&(_, first_z)) = under_from.first() else {
            return Route::OffMesh;
        };
        let hint = hint_z.unwrap_or(first_z);
        let nearest = under_from.iter().map(|&(_, z)| (z - hint).abs()).fold(f64::INFINITY, f64::min);
        let start_z = under_from.iter().find(|&&(_, z)| (z - hint).abs() <= nearest).map_or(first_z, |&(_, z)| z);

        let mut queue: Vec<usize> = under_from
            .iter()
            .filter(|&&(_, z)| (z - hint).abs() <= nearest + LAYER_TOLERANCE)
            .map(|&(slot, _)| slot)
            .collect();
        let mut seen = vec![false; touched.len()];
        for &slot in &queue {
            if let Some(flag) = seen.get_mut(slot) {
                *flag = true;
            }
        }

        let mut goal: Option<(f64, usize)> = None; // (distance from hint, triangle)
        let mut head = 0;
        while let Some(&slot) = queue.get(head) {
            head += 1;
            let Some(cur) = touched.get(slot).copied() else { continue };
            if cur.hi >= 1.0 {
                if let Some(z) = self.height_in(cur.tri, to) {
                    let score = (z - hint).abs();
                    if goal.is_none_or(|(best, _)| score < best) {
                        goal = Some((score, cur.tri));
                    }
                }
            }
            for &next in self.links.get(cur.tri).into_iter().flatten().flatten() {
                let Ok(next_slot) = touched.binary_search_by_key(&next, |t| t.tri) else { continue };
                let Some(n) = touched.get(next_slot).copied() else { continue };
                // The segment must really pass from one triangle into the other.
                if seen.get(next_slot).copied().unwrap_or(true) || n.lo > cur.hi || cur.lo > n.hi {
                    continue;
                }
                if let Some(flag) = seen.get_mut(next_slot) {
                    *flag = true;
                }
                queue.push(next_slot);
            }
        }

        match goal {
            Some((_, tri)) => {
                let (x, y, z) = self.settle(tri, to);
                Route::Reached { x, y, z }
            }
            None => Route::Blocked { z: start_z },
        }
    }

    /// Final accepted position for a destination `q` in triangle `index`: `q` itself, moved
    /// away from blocked edges if it is on one (see the module docs), with its surface height.
    pub(crate) fn settle(&self, index: usize, q: (f64, f64)) -> (f64, f64, f64) {
        let Some(tri) = self.tri(index) else {
            return (q.0, q.1, 0.0);
        };
        let z_at = |x: f64, y: f64| normalize(tri.raw_weights(x, y)).map_or_else(|| tri.centroid()[2], |w| tri.z_at(w));
        let keep = (q.0, q.1, z_at(q.0, q.1));
        let blocked = self.links.get(index).map_or([false; 3], |l| l.map(|n| n.is_none()));
        let dist_q = tri.edge_distances(q.0, q.1);
        if !dist_q.iter().zip(&blocked).any(|(&d, &b)| b && d < WALL_TRIGGER) {
            return keep;
        }

        // Move toward the centroid just far enough to clear every blocked edge.
        let c = tri.centroid();
        let dist_c = tri.edge_distances(c[0], c[1]);
        let mut lambda = 0.0_f64;
        for ((&dq, &dc), &is_blocked) in dist_q.iter().zip(&dist_c).zip(&blocked) {
            if is_blocked && dq < WALL_MARGIN {
                if dc <= WALL_MARGIN {
                    return keep; // a needle: the margin cannot be met inside it
                }
                lambda = lambda.max((WALL_MARGIN - dq) / (dc - dq));
            }
        }
        let (nx, ny) = (q.0 + lambda * (c[0] - q.0), q.1 + lambda * (c[1] - q.1));
        if (nx - q.0).hypot(ny - q.1) > MAX_NUDGE {
            return keep;
        }
        // Positions are stored as f32: judge the point that will actually be stored.
        let (nx, ny) = (f64::from(nx as f32), f64::from(ny as f32));
        let dist_n = tri.edge_distances(nx, ny);
        let clears_walls = dist_n.iter().zip(&blocked).all(|(&d, &b)| !b || d >= WALL_TRIGGER);
        let still_inside = dist_n.iter().all(|&d| d >= -EDGE_TOLERANCE);
        if clears_walls && still_inside {
            (nx, ny, z_at(nx, ny))
        } else {
            keep
        }
    }

    /// Up to `max` spawn points: centroids of the largest triangles of the largest connected
    /// region (connected through access links, in either direction), so that everyone who
    /// spawns can reach everyone else. Region size is total area; ties go to the region
    /// holding the lowest triangle index.
    pub(crate) fn spawn_points(&self, max: usize) -> Vec<Pos> {
        let n = self.tris.len();
        let mut parent: Vec<usize> = (0..n).collect();
        for (i, links) in self.links.iter().enumerate() {
            if self.tri(i).is_none() {
                continue;
            }
            for &j in links.iter().flatten() {
                if self.tri(j).is_some() {
                    union(&mut parent, i, j);
                }
            }
        }
        let roots: Vec<usize> = (0..n).map(|i| find(&mut parent, i)).collect();

        let mut area = vec![0.0_f64; n];
        let mut first = vec![usize::MAX; n];
        for (i, t) in self.usable() {
            let Some(&r) = roots.get(i) else { continue };
            if let Some(a) = area.get_mut(r) {
                *a += t.area2.abs();
            }
            if let Some(f) = first.get_mut(r) {
                *f = (*f).min(i);
            }
        }
        let area_of = |r: usize| area.get(r).copied().unwrap_or(0.0);
        let first_of = |r: usize| first.get(r).copied().unwrap_or(usize::MAX);
        let best = (0..n)
            .filter(|&r| first_of(r) != usize::MAX)
            .max_by(|&a, &b| area_of(a).total_cmp(&area_of(b)).then(first_of(b).cmp(&first_of(a))));
        let Some(best) = best else {
            return Vec::new();
        };

        let mut members: Vec<(usize, &Tri)> = self.usable().filter(|(i, _)| roots.get(*i) == Some(&best)).collect();
        // largest first, stable by index
        members.sort_by(|(i, a), (j, b)| b.area2.abs().total_cmp(&a.area2.abs()).then(i.cmp(j)));
        let mut spawns: Vec<Pos> = Vec::new();
        for (_, t) in members {
            if spawns.len() >= max {
                break;
            }
            let c = t.centroid();
            let p = Pos::new(c[0] as f32, c[1] as f32, c[2] as f32);
            if !spawns.contains(&p) {
                spawns.push(p);
            }
        }
        spawns
    }

    /// Check the access words against the layout assumptions (see [`LinkReport`]).
    pub(crate) fn link_report(&self) -> LinkReport {
        let mut report = LinkReport::default();
        for (i, (links, verts)) in self.links.iter().zip(&self.raw).enumerate() {
            for (e, link) in links.iter().enumerate() {
                let Some(&j) = link.as_ref() else {
                    report.blocked_edges += 1;
                    continue;
                };
                report.links += 1;
                if self.links.get(j).is_some_and(|back| back.contains(&Some(i))) {
                    report.symmetric += 1;
                }
                // edge e runs from vertex e to vertex e + 1
                let end = |k: usize| verts.get(k % 3).copied();
                if let (Some(p), Some(q), Some(target)) = (end(e), end(e + 1), self.raw.get(j)) {
                    if target.contains(&p) && target.contains(&q) {
                        report.adjacent += 1;
                    }
                }
            }
        }
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::field::NO_ACCESS;
    use std::collections::HashMap;

    type V = (i16, i16, i16);
    type T = [V; 3];

    fn mesh(tris: &[T], access: &[[u16; 3]]) -> Walkmesh {
        Walkmesh {
            triangles: tris.iter().map(|t| t.map(|(x, y, z)| Vertex { x, y, z })).collect(),
            access: access.to_vec(),
        }
    }

    /// Link every pair of triangles that share an edge, the way a well-formed walkmesh does;
    /// every other edge is a wall. `key` decides when two vertices are "the same": the full
    /// `(x, y, z)` (so stacked layers never link) or just `(x, y)` (so a floor and a platform
    /// that meet in the ground plane do).
    fn access_by(tris: &[T], key: fn(V) -> V) -> Vec<[u16; 3]> {
        type Edge = (V, V);
        let norm = |p: V, q: V| -> Edge {
            let (p, q) = (key(p), key(q));
            if p <= q {
                (p, q)
            } else {
                (q, p)
            }
        };
        let mut by_edge: HashMap<Edge, Vec<usize>> = HashMap::new();
        for (i, t) in tris.iter().enumerate() {
            for e in 0..3 {
                by_edge.entry(norm(t[e], t[(e + 1) % 3])).or_default().push(i);
            }
        }
        tris.iter()
            .enumerate()
            .map(|(i, t)| {
                let mut a = [NO_ACCESS; 3];
                for (e, slot) in a.iter_mut().enumerate() {
                    let others = by_edge.get(&norm(t[e], t[(e + 1) % 3])).into_iter().flatten().filter(|&&j| j != i);
                    if let Some(&j) = others.min() {
                        *slot = j as u16;
                    }
                }
                a
            })
            .collect()
    }

    fn auto_access(tris: &[T]) -> Vec<[u16; 3]> {
        access_by(tris, |v| v)
    }

    fn auto_access_xy(tris: &[T]) -> Vec<[u16; 3]> {
        access_by(tris, |(x, y, _)| (x, y, 0))
    }

    fn linked(tris: &[T]) -> Surface {
        Surface::new(&mesh(tris, &auto_access(tris)))
    }

    /// Remove every link between triangles `< split` and triangles `>= split`.
    fn wall_between(access: &mut [[u16; 3]], split: usize) {
        for (i, row) in access.iter_mut().enumerate() {
            for slot in row.iter_mut() {
                if *slot != NO_ACCESS && ((i < split) != (usize::from(*slot) < split)) {
                    *slot = NO_ACCESS;
                }
            }
        }
    }

    /// A rectangle `[x0, x1] x [y0, y1]` at height `z` as two triangles.
    fn rect(x0: i16, y0: i16, x1: i16, y1: i16, z: i16) -> [T; 2] {
        [[(x0, y0, z), (x1, y0, z), (x1, y1, z)], [(x0, y0, z), (x1, y1, z), (x0, y1, z)]]
    }

    /// `nx` x `ny` squares of `size` units at height `z`.
    fn grid(nx: i16, ny: i16, size: i16, z: i16) -> Vec<T> {
        let mut out = Vec::new();
        for cx in 0..nx {
            for cy in 0..ny {
                out.extend(rect(cx * size, cy * size, (cx + 1) * size, (cy + 1) * size, z));
            }
        }
        out
    }

    fn reached(r: Route) -> (f64, f64, f64) {
        match r {
            Route::Reached { x, y, z } => (x, y, z),
            other => panic!("expected Reached, got {other:?}"),
        }
    }

    /// The height `Route::Blocked` reports, asserting the route really was blocked.
    fn blocked(r: Route) -> f64 {
        match r {
            Route::Blocked { z } => z,
            other => panic!("expected Blocked, got {other:?}"),
        }
    }

    fn near(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    struct XorShift(u64);
    impl XorShift {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn unit(&mut self) -> f64 {
            (self.next() >> 11) as f64 / (1u64 << 53) as f64
        }
    }

    #[test]
    fn a_move_inside_one_triangle_is_accepted_with_interpolated_height() {
        // z = x + y over a right triangle
        let s = Surface::new(&mesh(&[[(0, 0, 0), (100, 0, 100), (0, 100, 100)]], &[[NO_ACCESS; 3]]));
        let (x, y, z) = reached(s.route((10.0, 10.0), Some(20.0), (30.0, 20.0)));
        assert_eq!((x, y), (30.0, 20.0));
        assert!(near(z, 50.0));
        assert_eq!(s.usable_count(), 1);
    }

    #[test]
    fn crossing_an_open_edge_is_accepted_and_a_blocked_edge_is_refused() {
        let tris = rect(0, 0, 100, 100, 0);
        // linked across the diagonal
        let open = linked(&tris);
        assert_eq!(reached(open.route((80.0, 20.0), Some(0.0), (20.0, 80.0))).0, 20.0);
        // the same two triangles with the diagonal marked as a wall
        let walled = Surface::new(&mesh(&tris, &[[NO_ACCESS; 3], [NO_ACCESS; 3]]));
        assert_eq!(blocked(walled.route((80.0, 20.0), Some(0.0), (20.0, 80.0))), 0.0);
        // still free to move inside either half
        assert!(matches!(walled.route((80.0, 20.0), Some(0.0), (90.0, 10.0)), Route::Reached { .. }));
    }

    #[test]
    fn climbing_through_a_blocked_edge_onto_a_raised_platform_is_refused() {
        // The reviewer's climb case: floor z = 0 over x 0..100, platform z = 100 over x
        // 100..200, the shared edge x = 100 blocked on both sides. A destination-only check
        // accepts (105, 50) at z = 100.
        let [f0, f1] = rect(0, 0, 100, 100, 0);
        let [p0, p1] = rect(100, 0, 200, 100, 100);
        let tris = [f0, f1, p0, p1];

        // z-aware linking never links floor and platform: the edge is blocked
        let s = linked(&tris);
        assert_eq!(blocked(s.route((95.0, 50.0), Some(0.0), (105.0, 50.0))), 0.0);
        // and the way back down
        assert!(near(blocked(s.route((105.0, 50.0), Some(100.0), (95.0, 50.0))), 100.0));
        // each side is still walkable on its own
        assert!(matches!(s.route((10.0, 10.0), Some(0.0), (90.0, 90.0)), Route::Reached { .. }));
        assert!(matches!(s.route((110.0, 10.0), Some(100.0), (190.0, 90.0)), Route::Reached { .. }));

        // positive control: link floor and platform across x = 100 and the same step is
        // accepted, landing on the platform's surface
        let open = Surface::new(&mesh(&tris, &auto_access_xy(&tris)));
        let (x, _, z) = reached(open.route((95.0, 50.0), Some(0.0), (105.0, 50.0)));
        assert_eq!(x, 105.0);
        assert!(near(z, 100.0), "a step across an open edge lands on the other triangle's surface");
    }

    #[test]
    fn a_linked_ramp_is_followed_and_height_follows_the_surface() {
        // ramp: z = x from 0 at x = 0 to 100 at x = 100
        let tris = [[(0, 0, 0), (100, 0, 100), (100, 100, 100)], [(0, 0, 0), (100, 100, 100), (0, 100, 0)]];
        let s = linked(&tris);
        let mut p = (5.0, 50.0, 5.0);
        for i in 1..=19 {
            let x = 5.0 + 5.0 * f64::from(i);
            let (nx, ny, nz) = reached(s.route((p.0, p.1), Some(p.2), (x, 50.0)));
            assert!((nz - nx).abs() < 1e-6, "height {nz} at x {nx}");
            p = (nx, ny, nz);
        }
    }

    #[test]
    fn walking_off_a_platform_onto_the_floor_below_is_refused() {
        // The reviewer's fall and stacked-layer cases: a platform at z = 100 over x 0..100
        // standing over a floor at z = 0 that runs x 0..200. Nothing links platform to floor.
        let tris: Vec<T> = [rect(0, 0, 100, 100, 100), rect(0, 0, 100, 100, 0), rect(100, 0, 200, 100, 0)].concat();
        let s = linked(&tris);
        // off the platform's x = 100 edge onto the floor: refused, height unchanged
        assert!(near(blocked(s.route((95.0, 50.0), Some(100.0), (105.0, 50.0))), 100.0));
        // stacked layers: from the platform, moving where the floor also is stays on the platform
        let (_, _, z) = reached(s.route((20.0, 20.0), Some(100.0), (60.0, 30.0)));
        assert!(near(z, 100.0));
        // from the floor, moving under the platform stays on the floor
        let (_, _, z) = reached(s.route((120.0, 20.0), Some(0.0), (60.0, 30.0)));
        assert!(near(z, 0.0));
        // and the layer is picked by the entity's height, not by triangle order
        let (_, _, z) = reached(s.route((20.0, 20.0), Some(1.0), (60.0, 30.0)));
        assert!(near(z, 0.0));
        // the floor's xy region under the platform is not reachable from the platform at all
        // by a destination that only the floor covers
        assert!(near(blocked(s.route((50.0, 50.0), Some(100.0), (150.0, 50.0))), 100.0));
    }

    #[test]
    fn a_segment_through_a_shared_vertex_between_linked_triangles_is_accepted() {
        // 4 x 4 grid, every edge linked; walk along the main diagonal (through every grid
        // vertex and along every cell's diagonal edge), along grid lines and across vertices
        let s = linked(&grid(4, 4, 100, 7));
        for (p, q) in [
            ((0.0, 0.0), (400.0, 400.0)),
            ((0.0, 100.0), (400.0, 100.0)),
            ((200.0, 0.0), (200.0, 400.0)),
            ((50.0, 50.0), (350.0, 350.0)),
            ((100.0, 100.0), (300.0, 300.0)),
            ((0.0, 200.0), (200.0, 0.0)),
            ((100.0, 300.0), (300.0, 100.0)),
        ] {
            let (x, y, z) = reached(s.route(p, Some(7.0), q));
            assert!((x - q.0).abs() < 0.05 && (y - q.1).abs() < 0.05, "{p:?} -> {q:?} landed at ({x}, {y})");
            assert!((z - 7.0).abs() < 1e-6);
        }
    }

    #[test]
    fn random_segments_over_a_fully_linked_grid_match_a_convex_oracle() {
        let s = linked(&grid(8, 8, 100, 0));
        let mut rng = XorShift(0x5EED_1234_ABCD_9876);
        let snap = |v: f64, rng: &mut XorShift| if rng.next() & 3 == 0 { (v / 50.0).round() * 50.0 } else { v };
        for _ in 0..4000 {
            let p = (snap(rng.unit() * 800.0, &mut rng), snap(rng.unit() * 800.0, &mut rng));
            let q = (snap(rng.unit() * 800.0, &mut rng), snap(rng.unit() * 800.0, &mut rng));
            let (x, y, _) = reached(s.route(p, Some(0.0), q));
            assert!((x - q.0).abs() <= 0.05 && (y - q.1).abs() <= 0.05, "{p:?} -> {q:?} landed at ({x}, {y})");
        }
        // outside the square: refused (or no starting triangle), never accepted
        for _ in 0..400 {
            let p = (rng.unit() * 800.0, rng.unit() * 800.0);
            let q = (800.1 + rng.unit() * 500.0, rng.unit() * 800.0);
            assert_eq!(blocked(s.route(p, Some(0.0), q)), 0.0, "{p:?} -> {q:?}");
            assert_eq!(s.route(q, Some(0.0), p), Route::OffMesh);
        }
    }

    #[test]
    fn a_wall_across_a_dense_mesh_stops_every_crossing_segment() {
        // 8 x 4 grid; remove every link across x = 400
        let tris = grid(8, 4, 100, 0);
        let mut access = auto_access(&tris);
        let left_of_wall = |k: usize| tris[k].iter().map(|v| i32::from(v.0)).sum::<i32>() < 3 * 400;
        let mut cut = 0;
        for (i, row) in access.iter_mut().enumerate() {
            for slot in row.iter_mut() {
                if *slot != NO_ACCESS && left_of_wall(i) != left_of_wall(usize::from(*slot)) {
                    *slot = NO_ACCESS;
                    cut += 1;
                }
            }
        }
        assert!(cut > 0);
        let s = Surface::new(&mesh(&tris, &access));
        let mut rng = XorShift(0xFEED_FACE_0123_4567);
        for _ in 0..2000 {
            let (py, qy) = (rng.unit() * 400.0, rng.unit() * 400.0);
            let left = (rng.unit() * 399.0, py);
            let right = (401.0 + rng.unit() * 398.0, qy);
            assert_eq!(blocked(s.route(left, Some(0.0), right)), 0.0, "{left:?} -> {right:?}");
            assert_eq!(blocked(s.route(right, Some(0.0), left)), 0.0);
            let left2 = (rng.unit() * 399.0, qy);
            assert!(matches!(s.route(left, Some(0.0), left2), Route::Reached { .. }));
        }
    }

    /// Three triangles around the vertex V = (0, 0): T0 (angles 0-120), T1 (120-240), T2
    /// (240-360). Each pair shares an edge incident to V.
    fn fan() -> [T; 3] {
        [
            [(0, 0, 0), (100, 0, 0), (-50, 87, 0)],
            [(0, 0, 0), (-50, 87, 0), (-50, -87, 0)],
            [(0, 0, 0), (-50, -87, 0), (100, 0, 0)],
        ]
    }

    #[test]
    fn a_link_cannot_be_used_to_skip_a_triangle_the_segment_crosses() {
        let tris = fan();
        // from T0 straight down through T1 into T2
        let (p, q) = ((-20.0, 50.0), (-20.0, -50.0));
        // fully linked: T0 -> T1 -> T2
        assert!(matches!(linked(&tris).route(p, Some(0.0), q), Route::Reached { .. }));
        // T0 <-> T2 stay linked directly, but T1 is walled off from both: the segment crosses
        // T1 and must be refused even though T0 and T2 are neighbours
        let mut access = auto_access(&tris);
        access[1] = [NO_ACCESS; 3];
        for row in [0usize, 2] {
            for slot in access[row].iter_mut() {
                if *slot == 1 {
                    *slot = NO_ACCESS;
                }
            }
        }
        assert_eq!(access[0].iter().filter(|&&a| a == 2).count(), 1, "T0 and T2 remain linked");
        let s = Surface::new(&mesh(&tris, &access));
        assert_eq!(blocked(s.route(p, Some(0.0), q)), 0.0);
        // but a segment that really goes T0 -> T2 over their shared edge is fine
        assert!(matches!(s.route((50.0, 10.0), Some(0.0), (50.0, -10.0)), Route::Reached { .. }));
    }

    #[test]
    fn passing_through_the_vertex_of_a_fan_is_allowed_between_linked_triangles() {
        let s = linked(&fan());
        // around V, T0 -> T2 -> T1
        assert!(matches!(s.route((50.0, 5.0), Some(0.0), (-20.0, -10.0)), Route::Reached { .. }));
        // exactly through V from T0's interior to T1's interior
        assert!(matches!(s.route((10.0, 10.0), Some(0.0), (-10.0, -10.0)), Route::Reached { .. }));
        // exactly through a grid vertex shared by six triangles, between triangles that are
        // not neighbours (opposite sectors): needs hops at one and the same point
        let g = linked(&grid(2, 2, 100, 0));
        assert!(matches!(g.route((60.0, 140.0), Some(0.0), (140.0, 60.0)), Route::Reached { .. }));
        assert!(matches!(g.route((60.0, 60.0), Some(0.0), (140.0, 140.0)), Route::Reached { .. }));
        assert!(matches!(g.route((100.0, 40.0), Some(0.0), (100.0, 160.0)), Route::Reached { .. }));
    }

    #[test]
    fn standing_on_a_wall_does_not_let_the_next_step_cross_it() {
        // two squares side by side with a wall at x = 100 between them
        let tris: Vec<T> = [rect(0, 0, 100, 100, 0), rect(100, 0, 200, 100, 0)].concat();
        let mut access = auto_access(&tris);
        wall_between(&mut access, 2);
        let s = Surface::new(&mesh(&tris, &access));

        // step exactly onto the wall from the left: accepted, but kept off the wall
        let (x, y, z) = reached(s.route((50.0, 50.0), Some(0.0), (100.0, 50.0)));
        assert!((100.0 - MAX_NUDGE..100.0 - WALL_TRIGGER).contains(&x), "x = {x}");
        assert!((y - 50.0).abs() < 0.01 && z == 0.0);
        // the follow-up step across the wall is refused
        assert_eq!(blocked(s.route((x, y), Some(z), (150.0, 50.0))), 0.0);
        // the same from the right-hand side
        let (x, y, z) = reached(s.route((150.0, 50.0), Some(0.0), (100.0, 50.0)));
        assert!(x > 100.0 + WALL_TRIGGER && x <= 100.0 + MAX_NUDGE, "x = {x}");
        assert_eq!(blocked(s.route((x, y), Some(z), (50.0, 50.0))), 0.0);
        // a hair beyond the wall (inside the tolerance) is accepted, pulled back inside
        let (x, _, _) = reached(s.route((50.0, 50.0), Some(0.0), (100.0005, 50.0)));
        assert!(x < 100.0 - WALL_TRIGGER);
        // a position well clear of every wall is not moved at all
        assert_eq!(reached(s.route((50.0, 50.0), Some(0.0), (60.0, 40.0))), (60.0, 40.0, 0.0));
        // moving again from a settled position is stable (no creeping)
        let (x1, y1, z1) = reached(s.route((50.0, 50.0), Some(0.0), (100.0, 50.0)));
        assert_eq!(reached(s.route((x1, y1), Some(z1), (x1, y1))), (x1, y1, z1));
    }

    #[test]
    fn walls_at_a_corner_and_open_edges_are_handled() {
        // one square: outer edges are walls, the diagonal is open
        let s = linked(&rect(0, 0, 100, 100, 0));
        // a position on the outer corner is moved inside, not out of the triangle
        let (x, y, _) = reached(s.route((50.0, 20.0), Some(0.0), (0.0, 0.0)));
        assert!(x > 0.0 && y >= 0.0 && x < 0.1 && y < 0.1, "({x}, {y})");
        // a position on the OPEN diagonal is not moved
        assert_eq!(reached(s.route((60.0, 20.0), Some(0.0), (50.0, 50.0))), (50.0, 50.0, 0.0));
    }

    #[test]
    fn needle_triangles_are_not_dragged_far_to_clear_a_wall() {
        // 2000 units long, 0.004 wide: the wall margin cannot be met inside it, so the
        // position is left exactly where it was rather than moved along the needle
        let s = Surface::new(&mesh(&[[(0, 0, 0), (2000, 0, 0), (2000, 1, 0)]], &[[NO_ACCESS; 3]]));
        let (x, y, _) = reached(s.route((1000.0, 0.5), Some(0.0), (1000.0, 0.499)));
        assert_eq!((x, y), (1000.0, 0.499));
    }

    #[test]
    fn spawns_come_from_the_largest_connected_region() {
        // a huge isolated triangle, plus a linked 20 x 20 block of small triangles that is
        // bigger in total: the block must host the spawns even though the lone triangle is the
        // single largest triangle
        let lone: T = [(5000, 5000, 0), (7000, 5000, 0), (5000, 7000, 0)];
        let block = grid(20, 20, 100, 0);
        let mut tris = vec![lone];
        tris.extend(block.iter().copied());
        let mut access = vec![[NO_ACCESS; 3]];
        for row in auto_access(&block) {
            access.push(row.map(|a| if a == NO_ACCESS { a } else { a + 1 }));
        }
        let s = Surface::new(&mesh(&tris, &access));
        let spawns = s.spawn_points(4);
        assert_eq!(spawns.len(), 4);
        assert!(spawns.iter().all(|p| p.x < 2000.0 && p.y < 2000.0), "{spawns:?}");
        // every spawn is walkable and they can all reach one another
        for a in &spawns {
            for b in &spawns {
                let r = s.route((f64::from(a.x), f64::from(a.y)), Some(f64::from(a.z)), (f64::from(b.x), f64::from(b.y)));
                assert!(matches!(r, Route::Reached { .. }), "{a:?} -> {b:?}");
            }
        }
        // nothing linked: the regions are single triangles and the largest one hosts the spawn
        let s = Surface::new(&mesh(&tris, &vec![[NO_ACCESS; 3]; tris.len()]));
        let spawns = s.spawn_points(4);
        assert_eq!(spawns.len(), 1);
        assert!(spawns[0].x > 5000.0);
        // equal regions: the one holding the lowest triangle index wins
        let twins: [T; 2] = [[(0, 0, 0), (10, 0, 0), (0, 10, 0)], [(500, 500, 0), (510, 500, 0), (500, 510, 0)]];
        let s = Surface::new(&mesh(&twins, &[[NO_ACCESS; 3]; 2]));
        assert_eq!(s.spawn_points(4), vec![Pos::new(10.0 / 3.0, 10.0 / 3.0, 0.0)]);
        // nothing usable, nothing to spawn
        assert!(Surface::new(&mesh(&[], &[])).spawn_points(4).is_empty());
        assert!(Surface::new(&mesh(&[[(1, 1, 0); 3]], &[[NO_ACCESS; 3]])).spawn_points(4).is_empty());
    }

    #[test]
    fn degenerate_triangles_keep_their_slot_so_links_stay_aligned() {
        // triangle 0 is a point; triangles 1 and 2 are real and linked to each other by index
        let tris: [T; 3] = [
            [(5, 5, 0), (5, 5, 0), (5, 5, 0)],
            [(0, 0, 0), (100, 0, 0), (100, 100, 0)],
            [(0, 0, 0), (100, 100, 0), (0, 100, 0)],
        ];
        let access = [[NO_ACCESS; 3], [NO_ACCESS, NO_ACCESS, 2], [1, NO_ACCESS, NO_ACCESS]];
        let s = Surface::new(&mesh(&tris, &access));
        assert_eq!(s.usable_count(), 2);
        assert!(matches!(s.route((80.0, 20.0), Some(0.0), (20.0, 80.0)), Route::Reached { .. }));
    }

    #[test]
    fn links_are_directed_exactly_as_stored() {
        // 0 -> 1 is open but 1 -> 0 is a wall
        let tris = rect(0, 0, 100, 100, 0);
        let s = Surface::new(&mesh(&tris, &[[NO_ACCESS, NO_ACCESS, 1], [NO_ACCESS; 3]]));
        assert!(matches!(s.route((80.0, 20.0), Some(0.0), (20.0, 80.0)), Route::Reached { .. }));
        assert_eq!(blocked(s.route((20.0, 80.0), Some(0.0), (80.0, 20.0))), 0.0);
    }

    #[test]
    fn out_of_range_and_self_links_are_walls_not_panics() {
        let tris = rect(0, 0, 100, 100, 0);
        let s = Surface::new(&mesh(&tris, &[[NO_ACCESS, NO_ACCESS, 9], [7, 0xFFFE, 1]]));
        assert_eq!(blocked(s.route((80.0, 20.0), Some(0.0), (20.0, 80.0))), 0.0);
        // a triangle linked to itself is not a way out either
        assert_eq!(blocked(s.route((20.0, 80.0), Some(0.0), (80.0, 20.0))), 0.0);
    }

    #[test]
    fn extreme_inputs_never_panic_and_are_never_accepted() {
        let s = linked(&grid(2, 2, 100, 0));
        for q in [(1e30, 1e30), (-1e30, 5.0), (f64::from(f32::MAX), 0.0), (5.0, f64::from(f32::MIN))] {
            assert_eq!(blocked(s.route((10.0, 10.0), Some(0.0), q)), 0.0);
            assert_eq!(s.route(q, Some(0.0), (10.0, 10.0)), Route::OffMesh);
        }
        // no hint at all: the surface under `from` decides the layer
        assert!(matches!(s.route((10.0, 10.0), None, (150.0, 150.0)), Route::Reached { .. }));
        // zero-length move
        assert!(matches!(s.route((10.0, 10.0), Some(0.0), (10.0, 10.0)), Route::Reached { .. }));
    }

    #[test]
    fn a_large_mesh_is_routed_quickly() {
        // 100 x 100 cells = 20 000 triangles; a segment across the whole mesh visits ~200
        let s = linked(&grid(100, 100, 30, 0));
        assert_eq!(s.usable_count(), 20_000);
        let start = std::time::Instant::now();
        for i in 0..50 {
            let f = f64::from(i);
            let (x, y, _) = reached(s.route((1.0 + f, 2.0), Some(0.0), (2990.0 - f, 2995.0)));
            assert!(x > 2900.0 && y > 2900.0);
        }
        assert!(start.elapsed() < std::time::Duration::from_secs(30), "took {:?}", start.elapsed());
    }

    /// Independent statement of the routing rule by brute force: sample the segment densely and
    /// require every sample to lie in a triangle that the previous sample was in or that one of
    /// those triangles links to. (It shares only `inside` and `links` with the real thing.)
    fn oracle(s: &Surface, p: (f64, f64), q: (f64, f64)) -> bool {
        let inside = |x: f64, y: f64| -> Vec<usize> {
            s.usable().filter(|(_, t)| t.inside(x, y).is_some()).map(|(i, _)| i).collect()
        };
        let mut cur = inside(p.0, p.1);
        let len = (q.0 - p.0).hypot(q.1 - p.1);
        let steps = (len / 0.2).ceil().max(1.0) as usize;
        for k in 1..=steps {
            if cur.is_empty() {
                return false;
            }
            let t = k as f64 / steps as f64;
            let here = inside(p.0 + t * (q.0 - p.0), p.1 + t * (q.1 - p.1));
            cur = here
                .into_iter()
                .filter(|n| cur.contains(n) || cur.iter().any(|c| s.links.get(*c).is_some_and(|l| l.contains(&Some(*n)))))
                .collect();
        }
        !cur.is_empty()
    }

    /// Distance from point `v` to the segment `p -> q`.
    fn dist_to_segment(v: (f64, f64), p: (f64, f64), q: (f64, f64)) -> f64 {
        let (dx, dy) = (q.0 - p.0, q.1 - p.1);
        let len2 = dx * dx + dy * dy;
        let t = if len2 == 0.0 { 0.0 } else { (((v.0 - p.0) * dx + (v.1 - p.1) * dy) / len2).clamp(0.0, 1.0) };
        (v.0 - (p.0 + t * dx)).hypot(v.1 - (p.1 + t * dy))
    }

    #[test]
    fn routing_agrees_with_a_dense_sampling_oracle_on_random_walled_grids() {
        let mut rng = XorShift(0xC0FF_EE00_DEAD_BEEF);
        let (mut accepted, mut refused) = (0, 0);
        for mesh_no in 0..40 {
            // 4 x 4 cells of 100 units, every internal edge linked, then ~35% of the links
            // turned into walls (usually both ways, sometimes only one)
            let tris = grid(4, 4, 100, 0);
            let mut access = auto_access(&tris);
            for i in 0..access.len() {
                for e in 0..3 {
                    let j = usize::from(access[i][e]);
                    if access[i][e] == NO_ACCESS || rng.unit() >= 0.35 {
                        continue;
                    }
                    access[i][e] = NO_ACCESS;
                    if rng.unit() < 0.8 {
                        for back in access[j].iter_mut().filter(|a| usize::from(**a) == i) {
                            *back = NO_ACCESS;
                        }
                    }
                }
            }
            let s = Surface::new(&mesh(&tris, &access));
            let vertices: Vec<(f64, f64)> = (0..=4).flat_map(|x| (0..=4).map(move |y| (f64::from(x) * 100.0, f64::from(y) * 100.0))).collect();
            for _ in 0..60 {
                let p = (rng.unit() * 400.0, rng.unit() * 400.0);
                let q = (rng.unit() * 400.0, rng.unit() * 400.0);
                // generic crossings only: passing within a unit of a grid vertex is the
                // degenerate case, covered by dedicated tests
                if vertices.iter().any(|&v| dist_to_segment(v, p, q) < 1.0) {
                    continue;
                }
                let got = matches!(s.route(p, Some(0.0), q), Route::Reached { .. });
                let want = oracle(&s, p, q);
                assert_eq!(got, want, "mesh {mesh_no}: {p:?} -> {q:?} (route says {got}, the oracle says {want})");
                // both endpoints are on the mesh and the mesh is a convex square, so every
                // refusal is a wall
                if got {
                    accepted += 1;
                } else {
                    refused += 1;
                }
            }
        }
        // the comparison must have exercised both outcomes, many times
        assert!(accepted > 200 && refused > 200, "accepted {accepted}, refused {refused}");
    }

    #[test]
    fn garbage_meshes_never_panic_and_never_accept_a_destination_off_the_mesh() {
        let mut rng = XorShift(0xBAD5_EED5_0BAD_C0DE);
        for _ in 0..300 {
            let n = 1 + (rng.next() % 12) as usize;
            let coord = |rng: &mut XorShift| -> i16 {
                match rng.next() % 6 {
                    0 => i16::MIN,
                    1 => i16::MAX,
                    2 => 0,
                    _ => (rng.next() % 400) as i16 - 200,
                }
            };
            let tris: Vec<T> = (0..n)
                .map(|_| [0, 1, 2].map(|_| (coord(&mut rng), coord(&mut rng), coord(&mut rng))))
                .collect();
            let access: Vec<[u16; 3]> = (0..n)
                .map(|_| [0, 1, 2].map(|_| if rng.next() % 3 < 1 { NO_ACCESS } else { (rng.next() % 20) as u16 }))
                .collect();
            let s = Surface::new(&mesh(&tris, &access));
            let _ = s.spawn_points(4);
            let _ = s.link_report();
            for _ in 0..40 {
                let pt = |rng: &mut XorShift| match rng.next() % 5 {
                    0 => (f64::from(f32::MAX), -f64::from(f32::MAX)),
                    1 => (0.0, 0.0),
                    _ => (rng.unit() * 600.0 - 300.0, rng.unit() * 600.0 - 300.0),
                };
                let (p, q) = (pt(&mut rng), pt(&mut rng));
                let hint = if rng.next() & 1 == 0 { Some(rng.unit() * 400.0 - 200.0) } else { None };
                if let Route::Reached { x, y, z } = s.route(p, hint, q) {
                    assert!(x.is_finite() && y.is_finite() && z.is_finite(), "{x} {y} {z}");
                    // never somewhere that is not on the mesh (the accepted point is the
                    // destination, possibly moved a hair inward)
                    assert!(s.locate(x, y, None).is_some(), "accepted ({x}, {y}) is off the mesh: {p:?} -> {q:?}");
                    assert!((x - q.0).abs() <= 0.06 && (y - q.1).abs() <= 0.06, "{p:?} -> {q:?} landed at ({x}, {y})");
                }
            }
        }
    }

    #[test]
    fn link_report_counts_symmetric_adjacent_and_blocked_edges() {
        let tris = rect(0, 0, 100, 100, 0);
        let good = Surface::new(&mesh(&tris, &auto_access(&tris))).link_report();
        assert_eq!(good, LinkReport { links: 2, symmetric: 2, adjacent: 2, blocked_edges: 4 });
        // a link that points at a triangle that is not a neighbour
        let far = [[(0, 0, 0), (10, 0, 0), (0, 10, 0)], [(500, 500, 0), (510, 500, 0), (500, 510, 0)]];
        let odd = Surface::new(&mesh(&far, &[[1, NO_ACCESS, NO_ACCESS], [NO_ACCESS; 3]])).link_report();
        assert_eq!(odd, LinkReport { links: 1, symmetric: 0, adjacent: 0, blocked_edges: 5 });
    }
}
