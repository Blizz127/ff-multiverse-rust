//! Path planning for bots: a way from one point of an area to another that stays on the walkable
//! surface, found with nothing but the adapter's own `validate_move`.
//!
//! A bot that steers straight at its goal and slides along whatever blocks it gets stuck in any
//! concave corner (the doorway of `md1stin`'s platform is one), and its zigzag along a wall makes
//! chords the server refuses (the server walks the straight line between two announced positions;
//! a client path that hugged a corner between them is not that line). So the bot plans: a grid over
//! the area's bounds, the centre of a cell is a node when a walker can stand there with some
//! clearance around it, a step between two neighbouring nodes is an edge when the straight segment
//! is walkable with that clearance on both sides, A* finds the way, and string pulling turns it into
//! a few long straight legs. Walking those legs is walking straight lines that stay clear of the
//! walls, which the server agrees to.
//!
//! **Levels.** A surface can have several levels over one spot: a walkway above another, the turns
//! of a spiral stair (the way down in `nmkin_2` of the first reactor is one: four levels, one over
//! the other, joined by long ramps). `validate_move(from, to)` judges the step on the level `from`
//! stands on, so the planner keeps the height with every node: a node is not a cell but a cell AND
//! the level a walker is on there, found by walking (each node is reached by a walkable segment
//! from the one before, starting at the walker's own height), and one cell can hold a node per
//! level. A planner that asked for the surface at one fixed height would plan over the wrong level.
//! The goal is a point in x and y only (an exit line is crossed on any level), so any level will do
//! at its end.
//!
//! The planner knows nothing about any game: only `validate_move`, `bounds` and, when the caller
//! passes one, a test for the segments it must not walk (the lines of the exits the bot does not
//! want). Its cost is a few thousand `validate_move` calls for an area of a few thousand units;
//! plan once per leg, not per frame.

use fmv_adapter_api::{AreaRuntime, MoveResult};
use fmv_core::Pos;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};

/// The grids the planner tries, coarsest first: the target number of cells along the longer side of the
/// area's bounds, and the clearance asked around a node and along an edge as a fraction of the cell
/// size. The coarse grid is cheap and keeps well clear of walls; where the surface is narrow (the
/// walkways of a reactor are about 50 units wide, and a cell of 48 with 24 units of clearance on each
/// side has no node on them) the finer ones find the way.
const TIERS: [(f32, f32); 3] = [(80.0, 0.5), (160.0, 0.4), (320.0, 0.4)];
/// Cell size limits (area units).
const MIN_CELL: f32 = 12.0;
const MAX_CELL: f32 = 60.0;
/// Samples along a segment are at most this far apart (area units).
const SAMPLE_SPACING: f32 = 6.0;
/// A point counts as on the surface when `validate_move` keeps it within this of where it was.
const ON_SURFACE_EPS: f32 = 0.75;
/// Most grid cells the planner will handle (a degenerate or huge area does not make it allocate
/// without bound).
const MAX_CELLS: usize = 400_000;
/// Most nodes the search expands before it gives up (an area with several levels has more nodes
/// than cells, so this is not the cell limit).
const MAX_EXPANSIONS: usize = 400_000;
/// How far (in cells) the start may be moved to reach a node, and how near the goal (in cells) a
/// node must be for the last leg to be tried from it.
const SNAP_RADIUS_CELLS: i64 = 6;
/// The nodes of one cell are told apart by height: two heights this far apart or more are two levels.
const LEVEL_STEP: f32 = 24.0;
/// A shortcut between two nodes must end at the height the later node has (within this), or it has
/// led onto another level than the one the plan is on.
const SAME_LEVEL: f32 = 12.0;

/// Segments the planner must not walk (the exit lines the bot does not want to cross): `true` for a
/// step `a -> b` that must not be taken.
pub type Forbidden<'a> = &'a dyn Fn(Pos, Pos) -> bool;

/// Is `p` on the walkable surface (not corrected elsewhere)?
fn on_surface(area: &dyn AreaRuntime, p: Pos) -> Option<Pos> {
    match area.validate_move(p, p) {
        MoveResult::Accepted(q) if q.is_finite() && q.dist_xy(p) <= ON_SURFACE_EPS => Some(q),
        _ => None,
    }
}

/// Can the straight segment `a -> b` be walked, each sample reached from the previous one by an
/// accepted step (the first from `a`, at `a`'s height), with `margin` clear on both sides of every
/// sample and no step one `forbidden` refuses? Returns the end point as the area puts it (its
/// height is the surface's).
fn segment_walkable(area: &dyn AreaRuntime, a: Pos, b: Pos, margin: f32, forbidden: Forbidden<'_>) -> Option<Pos> {
    let len = a.dist_xy(b);
    if !len.is_finite() {
        return None;
    }
    let n = ((len / SAMPLE_SPACING).ceil() as usize).max(1);
    let (ux, uy) = if len > 1e-6 { ((b.x - a.x) / len, (b.y - a.y) / len) } else { (1.0, 0.0) };
    let mut cur = a;
    for i in 1..=n {
        let t = i as f32 / n as f32;
        let want = Pos::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t, cur.z);
        match area.validate_move(cur, want) {
            MoveResult::Accepted(q) if q.is_finite() && q.dist_xy(want) <= ON_SURFACE_EPS => {
                if forbidden(cur, q) {
                    return None;
                }
                cur = q;
            }
            _ => return None,
        }
        if margin > 0.0 {
            for side in [-1.0_f32, 1.0] {
                let probe = Pos::new(cur.x - uy * margin * side, cur.y + ux * margin * side, cur.z);
                on_surface(area, probe)?;
            }
        }
    }
    Some(cur)
}

/// A node of the search: a grid cell and the level (see [`level_of`]) a walker stands on there.
type Key = (i64, i64, i64);

/// The level of height `z`.
fn level_of(z: f32) -> i64 {
    (z / LEVEL_STEP).round() as i64
}

/// What the search knows of a node.
#[derive(Debug, Clone, Copy)]
struct Node {
    /// Where a walker stands at the cell's centre on this level (its height is the surface's).
    pos: Pos,
    /// The cost of the best way found to it.
    g: f32,
    /// The node before it on that way (`None`: it was reached from the start point itself).
    parent: Option<Key>,
}

/// An entry of the open set, ordered for a min-heap on `f`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Open {
    f: f32,
    g: f32,
    key: Key,
}

impl Eq for Open {}

impl Ord for Open {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reversed: `BinaryHeap` is a max-heap. Ties break on the key so the order is total.
        other.f.total_cmp(&self.f).then_with(|| other.key.cmp(&self.key))
    }
}

impl PartialOrd for Open {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// The grid the planner works on.
struct Grid<'a> {
    area: &'a dyn AreaRuntime,
    origin: (f32, f32),
    cell: f32,
    cols: i64,
    rows: i64,
    margin: f32,
}

impl<'a> Grid<'a> {
    /// The grid of one of the [`TIERS`]: `cells` along the longer side of the bounds, `clearance` as a
    /// fraction of a cell.
    fn new(area: &'a dyn AreaRuntime, (cells, clearance): (f32, f32)) -> Option<Self> {
        let b = area.bounds();
        let (w, h) = (b.max.x - b.min.x, b.max.y - b.min.y);
        if !(w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0) {
            return None;
        }
        let cell = (w.max(h) / cells).clamp(MIN_CELL, MAX_CELL);
        let (cols, rows) = ((w / cell).ceil() as i64 + 1, (h / cell).ceil() as i64 + 1);
        if cols.checked_mul(rows).is_none_or(|n| n as usize > MAX_CELLS) {
            return None;
        }
        Some(Self { area, origin: (b.min.x, b.min.y), cell, cols, rows, margin: cell * clearance })
    }

    fn contains(&self, c: (i64, i64)) -> bool {
        c.0 >= 0 && c.1 >= 0 && c.0 < self.cols && c.1 < self.rows
    }

    fn centre(&self, c: (i64, i64)) -> Pos {
        Pos::new(self.origin.0 + c.0 as f32 * self.cell, self.origin.1 + c.1 as f32 * self.cell, 0.0)
    }

    fn cell_of(&self, p: Pos) -> (i64, i64) {
        let cx = ((p.x - self.origin.0) / self.cell).round() as i64;
        let cy = ((p.y - self.origin.1) / self.cell).round() as i64;
        (cx.clamp(0, self.cols - 1), cy.clamp(0, self.rows - 1))
    }

    /// Where a walker standing at `p` stands after walking to the centre of cell `c` (straight, at
    /// `p`'s height to begin with), if it can with `margin` clear on both sides.
    fn reach(&self, p: Pos, c: (i64, i64), margin: f32, forbidden: Forbidden<'_>) -> Option<Pos> {
        let centre = self.centre(c);
        segment_walkable(self.area, p, Pos::new(centre.x, centre.y, p.z), margin, forbidden)
    }

    /// Is there clearance on the four sides of a walker standing at `q`?
    fn clear_around(&self, q: Pos) -> bool {
        [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)]
            .iter()
            .all(|(dx, dy)| on_surface(self.area, Pos::new(q.x + dx * self.margin, q.y + dy * self.margin, q.z)).is_some())
    }
}

/// Is there already a node in cell `c`, at about the height `z` (within a cell: a slope of up to
/// 45 degrees), that was reached at no more than cost `g`? Then walking there again is no use.
fn has_better(nodes: &HashMap<Key, Node>, c: (i64, i64), z: f32, g: f32, reach: f32) -> bool {
    (level_of(z - reach)..=level_of(z + reach)).any(|l| nodes.get(&(c.0, c.1, l)).is_some_and(|n| n.g <= g && (n.pos.z - z).abs() <= reach))
}

/// A walkable way from `from` to `to`: waypoints (the first is `from`, the last is `to`) whose
/// straight legs keep clear of the surface's edge wherever the surface allows it, and that never
/// take a step `forbidden` refuses (the lines of the exits the walker does not want to cross, say).
/// `None` when no way exists on the planner's grids (the goal is cut off, or off the surface by more
/// than a few cells).
///
/// `from` is where the walker stands, at its own height; `to` is a point in x and y (its height is
/// not used: the plan may end on any level that has that point). The legs between the first waypoint
/// and the first node, and between the last node and the last waypoint, are held to a lower standard
/// (no clearance): the endpoints may well stand next to a wall.
pub fn plan_path_avoiding(area: &dyn AreaRuntime, from: Pos, to: Pos, forbidden: Forbidden<'_>) -> Option<Vec<Pos>> {
    if !(from.is_finite() && to.is_finite()) {
        return None;
    }
    // Straight is best when it works (with clearance, so it is also what the server accepts).
    let clearance = TIERS.first().and_then(|t| Grid::new(area, *t)).map_or(0.0, |g| g.margin);
    if segment_walkable(area, from, to, clearance, forbidden).is_some() {
        return Some(vec![from, to]);
    }
    // The coarsest grid that has a way. (A small area has the same grid for several tiers: it is searched once.)
    let mut tried: Option<(f32, f32)> = None;
    for tier in TIERS {
        let Some(grid) = Grid::new(area, tier) else { continue };
        if tried == Some((grid.cell, grid.margin)) {
            continue;
        }
        tried = Some((grid.cell, grid.margin));
        if let Some(plan) = plan_on(&grid, from, to, forbidden) {
            return Some(plan);
        }
    }
    None
}

/// The search on one grid: A* over the nodes of every level, then string pulling.
fn plan_on(grid: &Grid<'_>, from: Pos, to: Pos, forbidden: Forbidden<'_>) -> Option<Vec<Pos>> {
    let area = grid.area;
    let mut nodes: HashMap<Key, Node> = HashMap::new();
    let mut unclear: HashSet<Key> = HashSet::new();
    let mut open = BinaryHeap::new();

    // The first steps: from where the walker stands to the cell centres around it. The nearest ring
    // with a node settles it, one ring further out than the first that has one (a node a little
    // further may be the better one).
    let (cx, cy) = grid.cell_of(from);
    for r in 0..=SNAP_RADIUS_CELLS {
        let mut found = false;
        for dx in -r..=r {
            for dy in -r..=r {
                let c = (cx + dx, cy + dy);
                if dx.abs().max(dy.abs()) != r || !grid.contains(c) {
                    continue;
                }
                let Some(q) = grid.reach(from, c, 0.0, forbidden).filter(|q| grid.clear_around(*q)) else { continue };
                let (key, g) = ((c.0, c.1, level_of(q.z)), from.dist_xy(q));
                if nodes.get(&key).is_some_and(|n| n.g <= g) {
                    continue;
                }
                nodes.insert(key, Node { pos: q, g, parent: None });
                open.push(Open { f: g + q.dist_xy(to), g, key });
                found = true;
            }
        }
        if found && r >= 1 {
            break;
        }
    }

    // A*, over the nodes of every level.
    let goal_reach = grid.cell * SNAP_RADIUS_CELLS as f32;
    let mut expanded = 0usize;
    let mut goal: Option<Key> = None;
    while let Some(Open { g, key, .. }) = open.pop() {
        let Some(node) = nodes.get(&key).copied() else { continue };
        if g > node.g + 1e-3 {
            continue; // a way to this node that was beaten after it was queued
        }
        // Near the goal: the last leg is the weaker test (no clearance); a node it does not work from is just a node.
        if node.pos.dist_xy(to) <= goal_reach && segment_walkable(area, node.pos, to, 0.0, forbidden).is_some() {
            goal = Some(key);
            break;
        }
        expanded += 1;
        if expanded > MAX_EXPANSIONS {
            return None;
        }
        for (dx, dy) in [(1, 0), (-1, 0), (0, 1), (0, -1), (1, 1), (1, -1), (-1, 1), (-1, -1)] {
            let c = (key.0 + dx, key.1 + dy);
            if !grid.contains(c) {
                continue;
            }
            // Diagonals cut corners: the segment itself must be walkable, with the clearance.
            let guess = node.g + grid.centre(c).dist_xy(node.pos);
            if has_better(&nodes, c, node.pos.z, guess, grid.cell) {
                continue;
            }
            let Some(q) = grid.reach(node.pos, c, grid.margin, forbidden) else { continue };
            let (next, g_next) = ((c.0, c.1, level_of(q.z)), node.g + node.pos.dist_xy(q));
            if nodes.get(&next).is_some_and(|n| n.g <= g_next) || unclear.contains(&next) {
                continue;
            }
            if !grid.clear_around(q) {
                unclear.insert(next);
                continue;
            }
            nodes.insert(next, Node { pos: q, g: g_next, parent: Some(key) });
            open.push(Open { f: g_next + q.dist_xy(to), g: g_next, key: next });
        }
    }
    let goal = goal?;

    // The nodes from the start to the goal.
    let mut chain = Vec::new();
    let mut at = Some(goal);
    while let Some(k) = at {
        let n = nodes.get(&k)?;
        chain.push(n.pos);
        at = n.parent;
    }
    chain.reverse();

    // String pulling: from each anchor go as far ahead along the path as one straight, clear leg
    // reaches. The first and last legs only need to be walkable. A leg must also end on the level
    // of the node it is aimed at (the goal's level is free): a straight line that is walkable but
    // arrives on another level is not the way the path goes.
    let mut pts: Vec<Pos> = Vec::with_capacity(chain.len() + 2);
    pts.push(from);
    pts.extend(chain);
    pts.push(to);
    let last = pts.len() - 1;
    let leg = |i: usize, j: usize, margin: f32| -> Option<bool> {
        let (a, b) = (*pts.get(i)?, *pts.get(j)?);
        Some(segment_walkable(area, a, b, margin, forbidden).is_some_and(|end| j == last || (end.z - b.z).abs() <= SAME_LEVEL))
    };
    let mut waypoints = vec![from];
    let mut anchor = 0usize;
    while anchor < last {
        let mut reach = anchor + 1;
        // Always valid: consecutive nodes were joined by an edge (the first and last legs by
        // the weaker test below, which may fail: then the plan fails).
        for j in (anchor + 1)..=last {
            let m = if anchor == 0 || j == last { 0.0 } else { grid.margin };
            if leg(anchor, j, m)? {
                reach = j;
            } else if j == anchor + 1 {
                // Not even the next node can be reached with that clearance: ask less of the leg.
                if !leg(anchor, j, 0.0)? {
                    return None;
                }
                reach = j;
            } else {
                break;
            }
        }
        waypoints.push(*pts.get(reach)?);
        anchor = reach;
    }
    Some(waypoints)
}

/// The owner's real fields, for the `#[ignore]`d tests that need them. Read-only: `FMV_FF7_DATA` is the game's `data/`
/// directory (the one with `field/flevel.lgp`); nothing here keeps or prints game bytes.
#[cfg(test)]
pub(crate) mod real {
    use fmv_core::{EditionProvenance, GameId, Platform, Pos};
    use fmv_ff7::{Ff7Adapter, Ff7FieldArea};

    pub(crate) fn adapter() -> Option<Ff7Adapter> {
        let Some(dir) = std::env::var_os("FMV_FF7_DATA") else {
            eprintln!("skipped: FMV_FF7_DATA is not set");
            return None;
        };
        let provenance = EditionProvenance {
            game: GameId::Ff7,
            platform: Platform::Pc,
            edition: "steam".into(),
            region: "any".into(),
            language: "en".into(),
            revision: "test".into(),
            sources: vec![],
            modded: false,
        };
        Some(Ff7Adapter::from_flevel_path(std::path::Path::new(&dir).join("field").join("flevel.lgp"), provenance).unwrap())
    }

    /// `field` and where a walker stands in it after arriving from `from` (`""`: at the account's spawn).
    pub(crate) fn arrival(adapter: &Ff7Adapter, field: &str, from: &str) -> (Ff7FieldArea, Pos) {
        use fmv_adapter_api::AreaRuntime;
        let area = adapter.load_field(field).unwrap();
        let start = if from.is_empty() {
            area.place_arrival(area.spawn_points()[0], None)
        } else {
            let exit = adapter.load_field(from).unwrap().exits().into_iter().find(|e| e.target == field).unwrap();
            area.place_arrival(exit.arrival, exit.arrival_hint)
        };
        (area, start)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmv_adapter_api::{AreaInfo, Bounds, GameAdapter};
    use fmv_fixture::{FixtureAdapter, FIXTURE_AREA_KEY};

    fn plaza() -> Box<dyn AreaRuntime> {
        FixtureAdapter::new().load_area(FIXTURE_AREA_KEY).unwrap()
    }

    /// A plan with no step forbidden.
    fn plan_path(area: &dyn AreaRuntime, from: Pos, to: Pos) -> Option<Vec<Pos>> {
        plan_path_avoiding(area, from, to, &|_, _| false)
    }

    fn at(x: f32, y: f32) -> Pos {
        Pos::new(x, y, 0.0)
    }

    /// Independent walk of a plan: every leg is walked in short accepted steps.
    fn assert_walkable(area: &dyn AreaRuntime, plan: &[Pos]) {
        for leg in plan.windows(2) {
            let (a, b) = (leg[0], leg[1]);
            let n = (a.dist_xy(b) / 3.0).ceil().max(1.0) as usize;
            let mut cur = a;
            for i in 1..=n {
                let t = i as f32 / n as f32;
                let want = at(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t);
                match area.validate_move(cur, want) {
                    MoveResult::Accepted(q) => cur = q,
                    other => panic!("leg {a:?} -> {b:?} is blocked at {want:?}: {other:?}"),
                }
            }
        }
    }

    #[test]
    fn a_clear_way_is_one_straight_leg() {
        let area = plaza();
        let plan = plan_path(area.as_ref(), at(-300.0, 0.0), at(300.0, 100.0)).expect("a plan");
        assert_eq!(plan.len(), 2, "{plan:?}");
        assert_eq!((plan[0], plan[1].x, plan[1].y), (at(-300.0, 0.0), 300.0, 100.0));
    }

    #[test]
    fn a_way_around_a_corner_is_planned_and_every_leg_is_walkable() {
        // The plaza is L-shaped: the corner at (400, 0) cuts the straight line from the top left to
        // the end of the arm, which is off the mesh there.
        let area = plaza();
        let (from, to) = (at(-300.0, 280.0), at(680.0, -20.0));
        assert!(segment_walkable(area.as_ref(), from, to, 0.0, &|_, _| false).is_none(), "the straight line is blocked");
        let plan = plan_path(area.as_ref(), from, to).expect("a plan");
        assert!(plan.len() >= 3, "it has to bend: {plan:?}");
        assert_eq!((plan.first().copied(), plan.last().copied()), (Some(from), Some(to)));
        assert_walkable(area.as_ref(), &plan);
        // And it keeps off the corner: no waypoint sits on the edge of the notch (the planner asks
        // for half a cell, 6.9 units here, of clearance).
        for p in &plan[1..plan.len() - 1] {
            assert!(p.dist_xy(at(400.0, 0.0)) > 8.0, "a waypoint at the corner of the notch: {p:?}");
        }
        let length: f32 = plan.windows(2).map(|w| w[0].dist_xy(w[1])).sum();
        assert!(length < from.dist_xy(to) * 1.25, "a reasonable detour: {length} for {}", from.dist_xy(to));
    }

    #[test]
    fn a_goal_off_the_surface_or_unreachable_has_no_plan() {
        let area = plaza();
        assert!(plan_path(area.as_ref(), at(0.0, 0.0), at(5000.0, 5000.0)).is_none(), "far off the mesh");
        assert!(plan_path(area.as_ref(), at(f32::NAN, 0.0), at(0.0, 0.0)).is_none());
        assert!(plan_path(area.as_ref(), at(0.0, 0.0), at(f32::INFINITY, 0.0)).is_none());
    }

    #[test]
    fn a_start_next_to_a_wall_still_gets_a_plan() {
        let area = plaza();
        // Right on the edge of the plaza, and a goal in the arm.
        let plan = plan_path(area.as_ref(), at(-399.0, 299.0), at(680.0, -20.0)).expect("a plan");
        assert_walkable(area.as_ref(), &plan);
    }

    #[test]
    fn a_step_the_caller_forbids_is_never_planned() {
        // The way round the notch of the plaza is the only way to the arm; forbid crossing the line x = 400 below the
        // notch's corner, as the line of an exit the bot does not want would be, and there is none.
        let area = plaza();
        let (from, to) = (at(-300.0, 280.0), at(680.0, -20.0));
        let across = |a: Pos, b: Pos| (a.x < 400.0) != (b.x < 400.0);
        assert!(plan_path_avoiding(area.as_ref(), from, to, &across).is_none(), "the arm is cut off");
        let plan = plan_path_avoiding(area.as_ref(), from, to, &|_, _| false).expect("and without the restriction there is a way");
        assert_walkable(area.as_ref(), &plan);
        // A restriction that leaves a way round is walked round: no point of the plan is in the patch.
        let patch = at(0.0, 50.0);
        let in_patch = |a: Pos, b: Pos| a.dist_xy(patch) < 30.0 || b.dist_xy(patch) < 30.0;
        let (from, to) = (at(-300.0, 0.0), at(300.0, 100.0));
        assert!(plan_path(area.as_ref(), from, to).is_some_and(|p| p.len() == 2), "without the patch it is straight");
        let plan = plan_path_avoiding(area.as_ref(), from, to, &in_patch).expect("a way round the forbidden patch");
        assert!(plan.len() >= 3, "it has to bend round the patch: {plan:?}");
        for leg in plan.windows(2) {
            let n = (leg[0].dist_xy(leg[1]) / 2.0).ceil().max(1.0) as usize;
            for i in 0..=n {
                let t = i as f32 / n as f32;
                let p = at(leg[0].x + (leg[1].x - leg[0].x) * t, leg[0].y + (leg[1].y - leg[0].y) * t);
                assert!(p.dist_xy(patch) >= 28.0, "the plan crosses the patch at {p:?}: {plan:?}");
            }
        }
    }

    /// A surface of three levels over one spot, like the spiral stair of the first reactor in miniature: a walkway
    /// `A` (height 100) over the strip x 0..400, y 0..100; a floor `C` (height 0) under it that is also a bigger room
    /// (x 0..600, y 0..300); and a ramp `R` from the walkway's east end (height 100) down to the floor (height 0) at
    /// x 400..500. A step is judged on the level the walker stands on, as `validate_move` of the FF7 adapter judges
    /// it (a level is left only where another one joins it: heights within 15 units of the walker's), so the
    /// walkway can be left only by the ramp.
    struct Levels(AreaInfo);

    impl Levels {
        fn new() -> Self {
            Self(plaza().info().clone())
        }

        /// The heights of the surfaces over (x, y).
        fn heights(x: f32, y: f32) -> Vec<f32> {
            let mut h = Vec::new();
            if (0.0..=400.0).contains(&x) && (0.0..=100.0).contains(&y) {
                h.push(100.0);
            }
            if (0.0..=600.0).contains(&x) && (0.0..=300.0).contains(&y) {
                h.push(0.0);
            }
            if (400.0..=500.0).contains(&x) && (0.0..=100.0).contains(&y) {
                h.push(100.0 - (x - 400.0));
            }
            h
        }

        /// The surface at (x, y) the walker at height `z` is on: the nearest within 15 units.
        fn level(x: f32, y: f32, z: f32) -> Option<f32> {
            Self::heights(x, y).into_iter().filter(|h| (h - z).abs() <= 15.0).min_by(|a, b| (a - z).abs().total_cmp(&(b - z).abs()))
        }
    }

    impl AreaRuntime for Levels {
        fn info(&self) -> &AreaInfo {
            &self.0
        }
        fn spawn_points(&self) -> Vec<Pos> {
            vec![Pos::new(50.0, 20.0, 100.0)]
        }
        fn validate_move(&self, from: Pos, to: Pos) -> MoveResult {
            if !(from.is_finite() && to.is_finite()) {
                return MoveResult::Corrected(from);
            }
            let Some(mut z) = Self::level(from.x, from.y, from.z) else { return MoveResult::Corrected(self.spawn_points()[0]) };
            let n = (from.dist_xy(to) / 2.0).ceil().max(1.0) as usize;
            for i in 1..=n {
                let t = i as f32 / n as f32;
                match Self::level(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t, z) {
                    Some(h) => z = h,
                    None => return MoveResult::Corrected(from),
                }
            }
            MoveResult::Accepted(Pos::new(to.x, to.y, z))
        }
        fn bounds(&self) -> Bounds {
            Bounds { min: Pos::new(0.0, 0.0, 0.0), max: Pos::new(600.0, 300.0, 100.0) }
        }
        fn walk_triangles(&self) -> Vec<[Pos; 3]> {
            Vec::new()
        }
        fn max_speed(&self) -> f32 {
            600.0
        }
    }

    /// Walk `plan` as a bot does, from `from` and carrying the height from step to step; every step must be accepted.
    fn walk_from(area: &dyn AreaRuntime, from: Pos, plan: &[Pos]) -> Pos {
        let mut cur = from;
        for wp in &plan[1..] {
            let n = (cur.dist_xy(*wp) / 3.0).ceil().max(1.0) as usize;
            let start = cur;
            for i in 1..=n {
                let t = i as f32 / n as f32;
                let want = Pos::new(start.x + (wp.x - start.x) * t, start.y + (wp.y - start.y) * t, cur.z);
                match area.validate_move(cur, want) {
                    MoveResult::Accepted(q) => cur = q,
                    other => panic!("the plan leaves the surface on the way to {wp:?}, at {want:?} from {cur:?}: {other:?}"),
                }
            }
        }
        cur
    }

    #[test]
    fn a_way_between_levels_goes_by_the_ramp_and_every_step_of_it_is_walkable_from_the_walkway() {
        let area = Levels::new();
        // On the walkway, and a goal that only the floor has: the straight line leaves the walkway's edge.
        let (from, to) = (Pos::new(50.0, 20.0, 100.0), at(50.0, 250.0));
        assert!(segment_walkable(&area, from, to, 0.0, &|_, _| false).is_none(), "the straight line is blocked");
        let plan = plan_path(&area, from, to).expect("a plan by the ramp");
        assert_eq!((plan.first().copied(), plan.last().map(|p| (p.x, p.y))), (Some(from), Some((50.0, 250.0))));
        assert!(plan.iter().any(|p| p.x > 450.0), "it has to go down the ramp (x 400..500) and round: {plan:?}");
        let end = walk_from(&area, from, &plan);
        assert!(end.dist_xy(to) < 1.0 && end.z.abs() < 1.0, "it ends on the floor at the goal: {end:?}");
        // From the floor under the walkway to the same goal it is a straight line, whatever lies over it.
        let plan = plan_path(&area, at(50.0, 20.0), at(50.0, 250.0)).expect("a plan");
        assert_eq!(plan.len(), 2, "{plan:?}");
        // And the ramp is the only way: closing it (forbidding the steps over x = 450) leaves the goal out of reach.
        assert!(plan_path_avoiding(&area, from, to, &|a, b| (a.x < 450.0) != (b.x < 450.0)).is_none());
    }

    #[test]
    fn a_goal_is_a_point_in_x_and_y_whatever_level_the_walker_gets_there_on() {
        let area = Levels::new();
        // From the floor to the west end of the walkway's strip: the floor has that x and y too, and an exit line is
        // crossed on any level, so the floor's straight line is the plan.
        let plan = plan_path(&area, at(550.0, 200.0), at(20.0, 50.0)).expect("a plan");
        assert_eq!(plan.len(), 2, "{plan:?}");
        let end = walk_from(&area, at(550.0, 200.0), &plan);
        assert!(end.dist_xy(at(20.0, 50.0)) < 1.0 && end.z.abs() < 1.0, "{end:?}");
    }

    /// The way on from the arrival of the first reactor's fields, on the owner's REAL data (read-only; `#[ignore]`,
    /// `FMV_FF7_DATA` is the game's `data/` directory). `nmkin_2` is a stair of four levels, joined by long ramps and
    /// about 50 units wide: from the arrival a plan walks down all of it to the line to `nmkin_3` (the grid that keeps
    /// 24 units clear of the edge has no node on it, so a finer one finds the way; a planner that asked for the surface
    /// at height 0 for every node planned over the wrong level and got nowhere). Past `nmkin_3` there is no way on
    /// foot (the triangles of the walkmesh say so: `fmv-ff7`'s `chain_real`) and the planner agrees.
    #[test]
    #[ignore = "needs the owner's real FF7 data (FMV_FF7_DATA)"]
    fn the_stair_of_the_first_reactor_is_planned_level_by_level_and_past_nmkin_3_there_is_no_way() {
        let Some(adapter) = real::adapter() else { return };
        // (field, the field it is entered from, the exit it wants, whether there is a way on foot)
        let hops = [("nmkin_2", "elevtr1", "nmkin_3", true), ("nmkin_3", "nmkin_2", "nmkin_4", false), ("nmkin_4", "nmkin_3", "nmkin_5", false)];
        for (field, from, to, way) in hops {
            let (area, start) = real::arrival(&adapter, field, from);
            let exits = area.exits();
            let goal = exits.iter().find(|e| e.target == to).unwrap();
            let others = |a: Pos, b: Pos| exits.iter().any(|e| !e.same_line_as(goal) && e.is_crossed_by(a, b));
            // Aim a little past the line, along the way the walker comes at it (the travel bot does)
            let mid = goal.midpoint();
            let d = start.dist_xy(mid);
            let aim = Pos::new(mid.x + (mid.x - start.x) / d * 30.0, mid.y + (mid.y - start.y) / d * 30.0, mid.z);
            let plan = plan_path_avoiding(&area, start, aim, &others);
            println!("{field}: {} waypoints to the line to {to}", plan.as_ref().map_or(0, Vec::len));
            assert_eq!(plan.is_some(), way, "{field}: a way to the line to {to}");
            let Some(plan) = plan else { continue };
            // every step of it is accepted from where the walker stands, and some step crosses the line
            let (mut cur, mut crossed) = (start, false);
            for wp in &plan[1..] {
                let (from_wp, n) = (cur, (cur.dist_xy(*wp) / 3.0).ceil().max(1.0) as usize);
                for i in 1..=n {
                    let t = i as f32 / n as f32;
                    let want = Pos::new(from_wp.x + (wp.x - from_wp.x) * t, from_wp.y + (wp.y - from_wp.y) * t, cur.z);
                    let MoveResult::Accepted(q) = area.validate_move(cur, want) else { panic!("{field}: the plan leaves the surface at {want:?} from {cur:?}") };
                    crossed |= goal.is_crossed_by(cur, q);
                    cur = q;
                }
            }
            assert!(crossed, "{field}: walking the plan crosses the line to {to}");
            let levels: std::collections::BTreeSet<i64> = plan.iter().map(|p| level_of(p.z)).collect();
            if field == "nmkin_2" {
                assert!(plan.len() >= 10 && levels.len() >= 4, "the stair has its levels in the plan: {} waypoints on {} levels", plan.len(), levels.len());
            }
        }
    }
}
