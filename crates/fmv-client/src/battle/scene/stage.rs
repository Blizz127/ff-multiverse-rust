//! The stand-in battle location: a sky that glows at the horizon, a round arena floor in tiles,
//! and the ground around it fading into the haze, all as flat-coloured triangles.
//!
//! It is a stand-in for the game's own location models (which this machine would read from its own
//! battle archive): nothing about it comes from game data. Everything is drawn through the
//! frame's [`View`], so it is in perspective with the actors: the floor is a polar mesh on the
//! ground plane (`y = 0`), each triangle clipped against the near plane and projected on the CPU,
//! painter's order (the sky, then the floor, then the ground), no depth needed because the ground
//! never overlaps itself. Colours fade towards the horizon's with the distance from the camera
//! ("fog"), which is what makes the floor read as lying in front of a far sky.

use super::camera::{View, STAGE_NEAR};
use crate::render::{color, Vertex};

pub const SKY_TOP: [u8; 3] = [16, 22, 56];
pub const HORIZON: [u8; 3] = [128, 96, 126];
pub const GROUND: [u8; 3] = [52, 48, 66];
pub const TILE_A: [u8; 3] = [96, 100, 124];
pub const TILE_B: [u8; 3] = [82, 86, 110];
pub const TILE_CENTRE: [u8; 3] = [70, 74, 98];
pub const RIM: [u8; 3] = [176, 182, 208];

/// How far the ground goes (battle units from the arena's middle); beyond it is the haze.
const FAR: f32 = 48_000.0;
/// Fog: none nearer than this distance from the camera, complete beyond [`FOG_FAR`].
const FOG_NEAR: f64 = 11_000.0;
const FOG_FAR: f64 = 40_000.0;
const SECTORS: usize = 28;
const FLOOR_RINGS: usize = 5;
const GROUND_RINGS: usize = 12;
/// The rim is this much wider than the floor.
const RIM_RATIO: f32 = 1.07;
/// Smallest and largest arena, in battle units of radius.
const MIN_RADIUS: f32 = 4200.0;
const MAX_RADIUS: f32 = 12_000.0;
/// How far the arena's edge is from the actor farthest from the middle.
const MARGIN: f32 = 1500.0;

/// The round floor the battle is fought on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Arena {
    /// The middle, on the ground `(x, z)`.
    pub centre: [f32; 2],
    pub radius: f32,
}

impl Arena {
    /// The arena that holds all of `points` (ground positions `(x, z)`) with room to spare.
    pub fn around(points: impl Iterator<Item = [f32; 2]>) -> Self {
        let points: Vec<[f32; 2]> = points.filter(|p| p[0].is_finite() && p[1].is_finite()).collect();
        if points.is_empty() {
            return Self { centre: [0.0, 0.0], radius: MIN_RADIUS };
        }
        let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
        for p in &points {
            for k in 0..2 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        let centre = [(lo[0] + hi[0]) * 0.5, (lo[1] + hi[1]) * 0.5];
        let farthest = points.iter().map(|p| (p[0] - centre[0]).hypot(p[1] - centre[1])).fold(0.0, f32::max);
        Self { centre, radius: (farthest + MARGIN).clamp(MIN_RADIUS, MAX_RADIUS) }
    }
}

/// A corner in camera space with its colour (sRGB bytes as floats, so they can be mixed).
#[derive(Debug, Clone, Copy)]
struct Corner {
    cam: [f64; 3],
    rgb: [f32; 3],
}

fn lerp3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

fn as_f32(rgb: [u8; 3]) -> [f32; 3] {
    rgb.map(f32::from)
}

fn as_u8(rgb: [f32; 3]) -> [u8; 3] {
    rgb.map(|v| v.round().clamp(0.0, 255.0) as u8)
}

/// `polygon` cut to the half-space `z >= near` (Sutherland-Hodgman), colours interpolated along
/// the cut edges.
fn clip_near(polygon: &[Corner], near: f64) -> Vec<Corner> {
    let mut out = Vec::with_capacity(polygon.len() + 2);
    for (i, a) in polygon.iter().enumerate() {
        let b = &polygon[(i + 1) % polygon.len()];
        let (a_in, b_in) = (a.cam[2] >= near, b.cam[2] >= near);
        if a_in {
            out.push(*a);
        }
        if a_in != b_in {
            let t = (near - a.cam[2]) / (b.cam[2] - a.cam[2]);
            out.push(Corner {
                cam: [a.cam[0] + (b.cam[0] - a.cam[0]) * t, a.cam[1] + (b.cam[1] - a.cam[1]) * t, near],
                rgb: lerp3(a.rgb, b.rgb, t as f32),
            });
        }
    }
    out
}

struct Painter<'a> {
    view: &'a View,
    srgb: bool,
    out: Vec<Vertex>,
}

impl Painter<'_> {
    fn vertex(&mut self, ndc: [f32; 2], rgb: [f32; 3]) {
        self.out.push(Vertex { pos: ndc, color: color(as_u8(rgb), self.srgb) });
    }

    /// A triangle given in NDC.
    fn flat_triangle(&mut self, a: ([f32; 2], [f32; 3]), b: ([f32; 2], [f32; 3]), c: ([f32; 2], [f32; 3])) {
        for (pos, rgb) in [a, b, c] {
            self.vertex(pos, rgb);
        }
    }

    /// A screen-aligned rectangle with a vertical colour gradient (`top` and `bottom` NDC `y`).
    fn band(&mut self, top: f32, bottom: f32, top_rgb: [f32; 3], bottom_rgb: [f32; 3]) {
        let (tl, tr) = (([-1.0, top], top_rgb), ([1.0, top], top_rgb));
        let (bl, br) = (([-1.0, bottom], bottom_rgb), ([1.0, bottom], bottom_rgb));
        self.flat_triangle(tl, tr, br);
        self.flat_triangle(tl, br, bl);
    }

    /// A triangle of the world given in camera space: clipped against the near plane, projected.
    fn world_triangle(&mut self, a: Corner, b: Corner, c: Corner) {
        let polygon = if [a, b, c].iter().all(|k| k.cam[2] >= STAGE_NEAR) { vec![a, b, c] } else { clip_near(&[a, b, c], STAGE_NEAR) };
        if polygon.len() < 3 {
            return;
        }
        let projected: Vec<([f32; 2], [f32; 3])> = polygon
            .iter()
            .filter_map(|k| self.view.ndc(k.cam, STAGE_NEAR * 0.5).map(|ndc| (ndc, k.rgb)))
            .collect();
        if projected.len() < 3 || projected.iter().any(|(p, _)| !p[0].is_finite() || !p[1].is_finite()) {
            return;
        }
        for i in 1..projected.len() - 1 {
            self.flat_triangle(projected[0], projected[i], projected[i + 1]);
        }
    }
}

/// The fog factor, 0 (clear) to 1 (haze), at camera-space `c`.
fn fog(c: [f64; 3]) -> f32 {
    let distance = (c[0] * c[0] + c[1] * c[1] + c[2] * c[2]).sqrt();
    let t = ((distance - FOG_NEAR) / (FOG_FAR - FOG_NEAR)).clamp(0.0, 1.0);
    (t * t * (3.0 - 2.0 * t)) as f32
}

/// The triangles of the stage for `view`: the sky and haze first, then the arena floor and the
/// ground (unless there is no `arena`: a location model is the floor then), in painter's order, as
/// NDC vertices for a target that is (`srgb`) or is not sRGB.
pub fn vertices(view: &View, arena: Option<&Arena>, srgb: bool) -> Vec<Vertex> {
    let mut p = Painter { view, srgb, out: Vec::with_capacity(4096) };
    let haze = as_f32(HORIZON);

    // The haze fills the window; the sky is a gradient from the top down to the horizon over it.
    p.band(1.0, -1.0, haze, haze);
    if let Some(horizon) = view.horizon_ndc_y() {
        if horizon < 1.0 {
            let bottom = horizon.clamp(-1.0, 1.0);
            // Where the gradient has got to at the bottom of the band (all the way to the haze, unless
            // the horizon is below the window).
            let reached = if horizon < -1.0 { (1.0 - bottom) / (1.0 - horizon) } else { 1.0 };
            p.band(1.0, bottom, as_f32(SKY_TOP), lerp3(as_f32(SKY_TOP), haze, reached));
        }
    }

    let Some(arena) = arena else { return p.out };
    // The ground: a polar mesh around the arena's middle. Vertex (ring, sector) is at radius
    // `radii[ring]`; each cell is shaded by `shade(ring, sector)` (a colour) and then fogged.
    let rim_radius = arena.radius * RIM_RATIO;
    let mut radii: Vec<f32> = (0..=FLOOR_RINGS).map(|k| arena.radius * k as f32 / FLOOR_RINGS as f32).collect();
    radii.push(rim_radius);
    let first = rim_radius;
    radii.extend((1..=GROUND_RINGS).map(|j| first * (FAR / first).powf(j as f32 / GROUND_RINGS as f32)));
    let rings = radii.len();
    let mut grid: Vec<[f64; 3]> = Vec::with_capacity(rings * SECTORS);
    for &r in &radii {
        for s in 0..SECTORS {
            let angle = s as f32 / SECTORS as f32 * std::f32::consts::TAU;
            let world = [f64::from(arena.centre[0] + r * angle.cos()), 0.0, f64::from(arena.centre[1] + r * angle.sin())];
            grid.push(view.camera.to_camera(world));
        }
    }
    // Cell `ring` is between radii `ring` and `ring + 1`: the middle disc, the tile rings of the
    // floor, the rim, then the ground.
    let shade = |ring: usize, sector: usize| -> [u8; 3] {
        match ring {
            0 => TILE_CENTRE,
            r if r < FLOOR_RINGS => {
                if (r + sector).is_multiple_of(2) {
                    TILE_A
                } else {
                    TILE_B
                }
            }
            FLOOR_RINGS => RIM,
            _ => GROUND,
        }
    };
    for ring in 0..rings - 1 {
        for sector in 0..SECTORS {
            let next = (sector + 1) % SECTORS;
            let base = as_f32(shade(ring, sector));
            let corner = |r: usize, s: usize| {
                let cam = grid[r * SECTORS + s];
                Corner { cam, rgb: lerp3(base, haze, fog(cam)) }
            };
            let (a, b, c, d) = (corner(ring, sector), corner(ring, next), corner(ring + 1, next), corner(ring + 1, sector));
            p.world_triangle(a, b, c);
            p.world_triangle(a, c, d);
        }
    }
    p.out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::battle::scene::camera::{BattleCamera, NOMINAL_TAN_HALF_V};

    /// A camera that is low enough to show the horizon with the nominal lens.
    fn view(w: u32, h: u32) -> View {
        let camera = BattleCamera::look_at([11_000.0, -1_500.0, 2_600.0], [0.0, -300.0, -200.0]).unwrap();
        View { camera, width: w, height: h, zoom: f64::from(h) * 0.5 / NOMINAL_TAN_HALF_V, shift: [0.0, 0.0] }
    }

    fn arena() -> Arena {
        Arena { centre: [0.0, -200.0], radius: 5000.0 }
    }

    /// The colour a software rasteriser would leave at NDC point `at` after the painter's-order
    /// triangles (the last one covering the point wins, colours interpolated).
    fn colour_at(vertices: &[Vertex], at: [f32; 2]) -> Option<[f32; 3]> {
        let mut seen = None;
        // (`chunks_exact(3)` here is `as_chunks` on Rust 1.88+; see `avatar` for why it is not used.)
        #[allow(clippy::chunks_exact_to_as_chunks)]
        for tri in vertices.chunks_exact(3) {
            let (a, b, c) = (tri[0], tri[1], tri[2]);
            let det = (b.pos[1] - c.pos[1]) * (a.pos[0] - c.pos[0]) + (c.pos[0] - b.pos[0]) * (a.pos[1] - c.pos[1]);
            if det.abs() < 1e-12 {
                continue;
            }
            let l1 = ((b.pos[1] - c.pos[1]) * (at[0] - c.pos[0]) + (c.pos[0] - b.pos[0]) * (at[1] - c.pos[1])) / det;
            let l2 = ((c.pos[1] - a.pos[1]) * (at[0] - c.pos[0]) + (a.pos[0] - c.pos[0]) * (at[1] - c.pos[1])) / det;
            let l3 = 1.0 - l1 - l2;
            if l1 >= -1e-6 && l2 >= -1e-6 && l3 >= -1e-6 {
                let mix = |k: usize| a.color[k] * l1 + b.color[k] * l2 + c.color[k] * l3;
                seen = Some([mix(0), mix(1), mix(2)]);
            }
        }
        seen
    }

    fn bytes(rgb: [u8; 3]) -> [f32; 3] {
        rgb.map(|v| f32::from(v) / 255.0)
    }

    fn close(a: [f32; 3], b: [f32; 3], tol: f32) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() <= tol)
    }

    #[test]
    fn near_plane_clipping_keeps_what_is_in_front_and_interpolates_the_cut() {
        let corner = |z: f64, v: f32| Corner { cam: [z, 0.0, z], rgb: [v, v, v] };
        let near = 100.0;
        // Fully in front: untouched. Fully behind: nothing.
        assert_eq!(clip_near(&[corner(200.0, 0.0), corner(300.0, 0.0), corner(400.0, 0.0)], near).len(), 3);
        assert!(clip_near(&[corner(10.0, 0.0), corner(20.0, 0.0), corner(30.0, 0.0)], near).is_empty());
        // One corner in front: a triangle again, with its cut corners on the plane.
        let one = clip_near(&[corner(300.0, 100.0), corner(0.0, 0.0), corner(50.0, 0.0)], near);
        assert_eq!(one.len(), 3);
        assert!(one.iter().all(|c| c.cam[2] >= near - 1e-9));
        let cut: Vec<&Corner> = one.iter().filter(|c| (c.cam[2] - near).abs() < 1e-9).collect();
        assert_eq!(cut.len(), 2);
        // The colour at a cut is interpolated by distance along the edge: 300 -> 0 cut at 100 is 2/3 of the way.
        assert!(cut.iter().any(|c| (c.rgb[0] - 100.0 / 3.0).abs() < 1e-3), "{cut:?}");
        // Two in front: a quadrilateral.
        let two = clip_near(&[corner(300.0, 0.0), corner(400.0, 0.0), corner(0.0, 0.0)], near);
        assert_eq!(two.len(), 4);
    }

    #[test]
    fn the_stage_has_sky_above_the_horizon_floor_in_the_middle_and_haze_between() {
        let v = view(1100, 680);
        let shot = vertices(&v, Some(&arena()), false);
        assert!(!shot.is_empty() && shot.len().is_multiple_of(3));
        assert!(shot.iter().all(|p| p.pos[0].is_finite() && p.pos[1].is_finite() && p.pos[0].abs() < 1.0e4 && p.pos[1].abs() < 1.0e4));
        // The very top of the window is the top of the sky.
        let top = colour_at(&shot, [0.0, 0.999]).unwrap();
        assert!(close(top, bytes(SKY_TOP), 0.02), "{top:?}");
        // The sky gets lighter towards the horizon.
        let horizon = v.horizon_ndc_y().unwrap();
        assert!(horizon > 0.0 && horizon < 1.0);
        let near_horizon = colour_at(&shot, [0.0, horizon + 0.01]).unwrap();
        assert!(near_horizon[0] > top[0] + 0.15, "{near_horizon:?} vs {top:?}");
        // Just below the horizon it is haze, the colour of the horizon itself.
        let haze = colour_at(&shot, [0.0, horizon - 0.01]).unwrap();
        assert!(close(haze, bytes(HORIZON), 0.06), "{haze:?}");
        // The middle of the arena is the middle disc of its floor.
        let middle = colour_at(&shot, v.to_ndc([0.0, 0.0, -200.0]).unwrap()).unwrap();
        assert!(close(middle, bytes(TILE_CENTRE), 0.03), "{middle:?}");
    }

    #[test]
    fn the_floor_is_tiled_in_two_shades_and_has_a_bright_rim() {
        let v = view(1100, 680);
        let shot = vertices(&v, Some(&arena()), false);
        // Sample the floor on rings of ground points: both tile shades occur, and so does the rim.
        let mut shades = std::collections::BTreeSet::new();
        let mut saw_rim = false;
        for k in 0..400 {
            let angle = k as f64 * 0.0157;
            for r in [1200.0, 2400.0, 3600.0, 4600.0] {
                let ground = [r * angle.cos(), 0.0, -200.0 + r * angle.sin()];
                if let Some(ndc) = v.to_ndc(ground) {
                    if ndc[0].abs() < 0.98 && ndc[1].abs() < 0.98 {
                        if let Some(c) = colour_at(&shot, ndc) {
                            for (name, tile) in [("a", TILE_A), ("b", TILE_B)] {
                                if close(c, bytes(tile), 0.03) {
                                    shades.insert(name);
                                }
                            }
                        }
                    }
                }
            }
            let edge = [5100.0 * angle.cos(), 0.0, -200.0 + 5100.0 * angle.sin()];
            if let Some(ndc) = v.to_ndc(edge).filter(|n| n[0].abs() < 0.98 && n[1].abs() < 0.98) {
                if let Some(c) = colour_at(&shot, ndc) {
                    saw_rim |= c[0] > 0.55;
                }
            }
        }
        assert_eq!(shades.len(), 2, "both tile shades are visible: {shades:?}");
        assert!(saw_rim, "the rim is brighter than any tile");
    }

    #[test]
    fn the_floor_fades_into_the_haze_with_distance() {
        let v = view(1100, 680);
        let shot = vertices(&v, Some(&arena()), false);
        // Ground points on the line away from the camera: further is closer to the haze colour.
        let haze = bytes(HORIZON);
        let distance_from_haze = |ground: [f64; 3]| {
            let ndc = v.to_ndc(ground).unwrap();
            let c = colour_at(&shot, ndc).unwrap();
            (c[0] - haze[0]).abs() + (c[1] - haze[1]).abs() + (c[2] - haze[2]).abs()
        };
        let near = distance_from_haze([9000.0, 0.0, 0.0]);
        let mid = distance_from_haze([-12_000.0, 0.0, -2000.0]);
        let far = distance_from_haze([-45_000.0, 0.0, -2000.0]);
        assert!(near > mid && mid > far, "{near} {mid} {far}");
        assert!(far < 0.05, "the far ground is haze: {far}");
    }

    #[test]
    fn a_camera_inside_the_ground_or_looking_straight_down_still_makes_a_valid_stage() {
        let straight_down = BattleCamera::look_at([0.0, -9000.0, 0.0], [0.0, 0.0, 0.0]).unwrap();
        let v = View { camera: straight_down, width: 640, height: 480, zoom: 900.0, shift: [0.0, 10.0] };
        let shot = vertices(&v, Some(&arena()), false);
        assert!(shot.iter().all(|p| p.pos[0].is_finite() && p.pos[1].is_finite()));
        // No horizon, so no sky; the floor is under the lens axis.
        assert!(v.horizon_ndc_y().is_none());
        assert!(colour_at(&shot, [0.0, 0.0]).is_some());
        // A low camera near the ground, looking along it: everything is clipped sanely.
        let low = BattleCamera::look_at([7000.0, -150.0, 0.0], [0.0, -150.0, 0.0]).unwrap();
        let v = View { camera: low, width: 640, height: 480, zoom: 900.0, shift: [0.0, 0.0] };
        let shot = vertices(&v, Some(&arena()), false);
        assert!(shot.iter().all(|p| p.pos[0].is_finite() && p.pos[1].is_finite()));
        // The sky is the upper half and the ground the lower, level with the lens axis.
        assert!(close(colour_at(&shot, [0.0, 0.95]).unwrap(), bytes(SKY_TOP), 0.25));
        // A camera looking up has sky everywhere and no ground to draw.
        let up = BattleCamera::look_at([5000.0, -300.0, 0.0], [0.0, -4000.0, 0.0]).unwrap();
        let v = View { camera: up, width: 640, height: 480, zoom: 900.0, shift: [0.0, 0.0] };
        let shot = vertices(&v, Some(&arena()), false);
        assert!(shot.iter().all(|p| p.pos[0].is_finite() && p.pos[1].is_finite()));
        assert!(colour_at(&shot, [0.0, 0.0]).is_some());
    }

    #[test]
    fn the_arena_holds_every_actor_and_stays_within_its_bounds() {
        // Round and centred on what it holds, with the margin.
        let a = Arena::around([[-2000.0, 0.0], [2000.0, 0.0]].into_iter());
        assert_eq!(a.centre, [0.0, 0.0]);
        assert_eq!(a.radius, (2000.0 + MARGIN).max(MIN_RADIUS));
        let wide = Arena::around([[-9000.0, -9000.0], [9000.0, 9000.0]].into_iter());
        assert_eq!(wide.radius, MAX_RADIUS, "never larger than the cap");
        for points in [vec![[-3000.0, -6000.0], [3000.0, 3000.0], [0.0, 2000.0]], vec![[100.0, 100.0]], vec![[7000.0, -2000.0], [-5000.0, 400.0]]] {
            let a = Arena::around(points.iter().copied());
            for p in &points {
                let d = (p[0] - a.centre[0]).hypot(p[1] - a.centre[1]);
                assert!(d + MARGIN <= a.radius + 1e-3, "{p:?} is a margin inside {a:?}");
            }
        }
        // Nothing, or nonsense, is the default arena.
        assert_eq!(Arena::around(std::iter::empty()), Arena { centre: [0.0, 0.0], radius: MIN_RADIUS });
        assert_eq!(Arena::around([[f32::NAN, 0.0]].into_iter()).radius, MIN_RADIUS);
    }

    #[test]
    fn an_srgb_target_gets_converted_colours_and_the_vertex_budget_holds() {
        let v = view(800, 600);
        let plain = vertices(&v, Some(&arena()), false);
        let srgb = vertices(&v, Some(&arena()), true);
        assert_eq!(plain.len(), srgb.len());
        // The haze's blue is darker in linear light.
        assert!(srgb[0].color[2] < plain[0].color[2]);
        assert!(plain.len() < 20_000, "the stage is a few thousand triangles at most: {}", plain.len());
        assert!(plain.len() <= crate::render::MAX_VERTICES);
    }
}
