//! Path planning for bots: a way from one point of an area to another that stays on the walkable
//! surface, found with nothing but the adapter's own `validate_move`.
//!
//! A bot that steers straight at its goal and slides along whatever blocks it gets stuck in any
//! concave corner (the doorway of `md1stin`'s platform is one), and its zigzag along a wall makes
//! chords the server refuses (the server walks the straight line between two announced positions;
//! a client path that hugged a corner between them is not that line). So the bot plans: a grid over
//! the area's bounds, the cells whose centre is on the surface with some clearance around it are
//! the nodes, a step between two neighbouring nodes is an edge when the straight segment is
//! walkable with that clearance on both sides, A* finds the way, and string pulling turns it into
//! a few long straight legs. Walking those legs is walking straight lines that stay clear of the
//! walls, which the server agrees to.
//!
//! The planner knows nothing about any game: only `validate_move` and `bounds`. Its cost is a few
//! thousand `validate_move` calls for an area of a few thousand units; plan once per leg, not
//! per frame.

use fmv_adapter_api::{AreaRuntime, MoveResult};
use fmv_core::Pos;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

/// Target number of grid cells along the longer side of the area's bounds.
const CELLS_ALONG_LONGER_SIDE: f32 = 80.0;
/// Cell size limits (area units).
const MIN_CELL: f32 = 12.0;
const MAX_CELL: f32 = 60.0;
/// Clearance asked around a node and along an edge, as a fraction of the cell size.
const CLEARANCE_FRACTION: f32 = 0.5;
/// Samples along a segment are at most this far apart (area units).
const SAMPLE_SPACING: f32 = 6.0;
/// A point counts as on the surface when `validate_move` keeps it within this of where it was.
const ON_SURFACE_EPS: f32 = 0.75;
/// Most grid cells the planner will handle (a degenerate or huge area does not make it allocate
/// without bound).
const MAX_CELLS: usize = 400_000;
/// How far (in cells) the start and the goal may be moved to reach a free cell.
const SNAP_RADIUS_CELLS: i64 = 6;

/// Is `p` on the walkable surface (not corrected elsewhere)?
fn on_surface(area: &dyn AreaRuntime, p: Pos) -> Option<Pos> {
    match area.validate_move(p, p) {
        MoveResult::Accepted(q) if q.is_finite() && q.dist_xy(p) <= ON_SURFACE_EPS => Some(q),
        _ => None,
    }
}

/// Can the straight segment `a -> b` be walked, each sample reached from the previous one by an
/// accepted step, with `margin` clear on both sides of every sample? Returns the end point as the
/// area puts it (its height is the surface's).
fn segment_walkable(area: &dyn AreaRuntime, a: Pos, b: Pos, margin: f32) -> Option<Pos> {
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
            MoveResult::Accepted(q) if q.is_finite() && q.dist_xy(want) <= ON_SURFACE_EPS => cur = q,
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

/// A node of the open set, ordered for a min-heap on `f`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Open {
    f: f32,
    cell: (i64, i64),
}

impl Eq for Open {}

impl Ord for Open {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reversed: `BinaryHeap` is a max-heap. Ties break on the cell so the order is total.
        other.f.total_cmp(&self.f).then_with(|| other.cell.cmp(&self.cell))
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
    /// Walkability of cell centres (with clearance), computed on demand.
    free: HashMap<(i64, i64), Option<Pos>>,
}

impl<'a> Grid<'a> {
    fn new(area: &'a dyn AreaRuntime) -> Option<Self> {
        let b = area.bounds();
        let (w, h) = (b.max.x - b.min.x, b.max.y - b.min.y);
        if !(w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0) {
            return None;
        }
        let cell = (w.max(h) / CELLS_ALONG_LONGER_SIDE).clamp(MIN_CELL, MAX_CELL);
        let (cols, rows) = ((w / cell).ceil() as i64 + 1, (h / cell).ceil() as i64 + 1);
        if cols.checked_mul(rows).is_none_or(|n| n as usize > MAX_CELLS) {
            return None;
        }
        Some(Self { area, origin: (b.min.x, b.min.y), cell, cols, rows, margin: cell * CLEARANCE_FRACTION, free: HashMap::new() })
    }

    fn centre(&self, c: (i64, i64)) -> Pos {
        Pos::new(self.origin.0 + c.0 as f32 * self.cell, self.origin.1 + c.1 as f32 * self.cell, 0.0)
    }

    fn cell_of(&self, p: Pos) -> (i64, i64) {
        let cx = ((p.x - self.origin.0) / self.cell).round() as i64;
        let cy = ((p.y - self.origin.1) / self.cell).round() as i64;
        (cx.clamp(0, self.cols - 1), cy.clamp(0, self.rows - 1))
    }

    /// The centre of `c` as the surface has it, when the cell is a node: on the surface and with
    /// clearance on its four sides.
    fn node(&mut self, c: (i64, i64)) -> Option<Pos> {
        if c.0 < 0 || c.1 < 0 || c.0 >= self.cols || c.1 >= self.rows {
            return None;
        }
        if let Some(known) = self.free.get(&c) {
            return *known;
        }
        let p = self.centre(c);
        let q = on_surface(self.area, p).filter(|_| {
            [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)]
                .iter()
                .all(|(dx, dy)| on_surface(self.area, Pos::new(p.x + dx * self.margin, p.y + dy * self.margin, p.z)).is_some())
        });
        self.free.insert(c, q);
        q
    }

    /// The nearest node to `p`, searching outward ring by ring.
    fn nearest_node(&mut self, p: Pos) -> Option<(i64, i64)> {
        let (cx, cy) = self.cell_of(p);
        let mut best: Option<(f32, (i64, i64))> = None;
        for r in 0..=SNAP_RADIUS_CELLS {
            for dx in -r..=r {
                for dy in -r..=r {
                    if dx.abs().max(dy.abs()) != r {
                        continue;
                    }
                    let c = (cx + dx, cy + dy);
                    if self.node(c).is_some() {
                        let d = self.centre(c).dist_xy(p);
                        if best.is_none_or(|(b, _)| d < b) {
                            best = Some((d, c));
                        }
                    }
                }
            }
            // A node found in ring r can only be beaten by one of ring r + 1 that is nearer, which
            // is possible only a little (corners); one more ring settles it.
            if best.is_some() && r >= 1 {
                break;
            }
        }
        best.map(|(_, c)| c)
    }
}

/// A walkable way from `from` to `to`: waypoints (the first is `from`, the last is `to`) whose
/// straight legs keep clear of the surface's edge wherever the surface allows it. `None` when no
/// way exists on the planner's grid (the goal is cut off, or off the surface by more than a few
/// cells).
///
/// The legs between the first waypoint and the first node, and between the last node and the last
/// waypoint, are held to a lower standard (no clearance): the endpoints may well stand next to a
/// wall.
pub fn plan_path(area: &dyn AreaRuntime, from: Pos, to: Pos) -> Option<Vec<Pos>> {
    if !(from.is_finite() && to.is_finite()) {
        return None;
    }
    // Straight is best when it works (with clearance, so it is also what the server accepts).
    let clearance = Grid::new(area).map_or(0.0, |g| g.margin);
    if segment_walkable(area, from, to, clearance).is_some() {
        return Some(vec![from, to]);
    }
    let mut grid = Grid::new(area)?;
    let start = grid.nearest_node(from)?;
    let goal = grid.nearest_node(to)?;

    // A*.
    let mut open = BinaryHeap::new();
    let mut g_cost: HashMap<(i64, i64), f32> = HashMap::new();
    let mut came_from: HashMap<(i64, i64), (i64, i64)> = HashMap::new();
    g_cost.insert(start, 0.0);
    let goal_pos = grid.centre(goal);
    open.push(Open { f: grid.centre(start).dist_xy(goal_pos), cell: start });
    let mut expanded = 0usize;
    while let Some(Open { cell, .. }) = open.pop() {
        if cell == goal {
            break;
        }
        expanded += 1;
        if expanded > MAX_CELLS {
            return None;
        }
        let g_here = *g_cost.get(&cell)?;
        let here = grid.node(cell)?;
        for (dx, dy) in [(1, 0), (-1, 0), (0, 1), (0, -1), (1, 1), (1, -1), (-1, 1), (-1, -1)] {
            let next = (cell.0 + dx, cell.1 + dy);
            let Some(there) = grid.node(next) else { continue };
            let step = here.dist_xy(there);
            let g_next = g_here + step;
            if g_cost.get(&next).is_some_and(|&g| g <= g_next) {
                continue;
            }
            // Diagonals cut corners: the segment itself must be walkable, with the clearance.
            if segment_walkable(grid.area, here, there, grid.margin).is_none() {
                continue;
            }
            g_cost.insert(next, g_next);
            came_from.insert(next, cell);
            open.push(Open { f: g_next + there.dist_xy(goal_pos), cell: next });
        }
    }
    if !came_from.contains_key(&goal) && start != goal {
        return None;
    }
    let mut cells = vec![goal];
    let mut at = goal;
    while at != start {
        at = *came_from.get(&at)?;
        cells.push(at);
    }
    cells.reverse();

    // String pulling: from each anchor go as far ahead along the path as one straight, clear leg
    // reaches. The first and last legs only need to be walkable.
    let mut nodes: Vec<Pos> = Vec::with_capacity(cells.len() + 2);
    nodes.push(from);
    nodes.extend(cells.iter().filter_map(|c| grid.node(*c)));
    nodes.push(to);
    let last = nodes.len() - 1;
    let margin_for = |i: usize, j: usize, m: f32| if i == 0 || j == last { 0.0 } else { m };
    let mut waypoints = vec![from];
    let mut anchor = 0usize;
    while anchor < last {
        let mut reach = anchor + 1;
        // Always valid: consecutive nodes were joined by an edge (the first and last legs by
        // the weaker test below, which may fail: then the plan fails).
        for j in (anchor + 1)..=last {
            let m = margin_for(anchor, j, grid.margin);
            let (a, b) = (*nodes.get(anchor)?, *nodes.get(j)?);
            if segment_walkable(area, a, b, m).is_some() {
                reach = j;
            } else if j == anchor + 1 {
                // Not even the next node can be reached with that clearance: ask less of the leg.
                if segment_walkable(area, a, b, 0.0).is_none() {
                    return None;
                }
                reach = j;
            } else {
                break;
            }
        }
        waypoints.push(*nodes.get(reach)?);
        anchor = reach;
    }
    Some(waypoints)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmv_adapter_api::GameAdapter;
    use fmv_fixture::{FixtureAdapter, FIXTURE_AREA_KEY};

    fn plaza() -> Box<dyn AreaRuntime> {
        FixtureAdapter::new().load_area(FIXTURE_AREA_KEY).unwrap()
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
        assert!(segment_walkable(area.as_ref(), from, to, 0.0).is_none(), "the straight line is blocked");
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
}
