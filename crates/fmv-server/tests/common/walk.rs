//! Shared owned-field walking probe: every proposed step is validated by the real adapter.

use std::collections::{HashMap, VecDeque};
use fmv_adapter_api::{AreaRuntime, Exit, MoveResult};
use fmv_core::Pos;

/// The steps that take a walker from `from` across (a line of) `exit`: breadth-first over a grid
/// of `CELL`-unit cells, one straight step to the centre of a neighbouring cell at a time, every
/// step one the area accepts unchanged (not slid along a wall), until a step crosses a line that
/// leads where `exit` leads. A step that would cross the line of another exit is not taken (it
/// would take the walker elsewhere). The last position of the route is the crossing step.
///
/// The client's bots plan the same way (`nav::plan_path_avoiding` in fmv-client, which also pulls the path
/// straight); this is the small stand-in a server test can have without any client code. A walker
/// that steers straight at the exit and slides along what blocks it gets stuck in concave places
/// and makes chords the server refuses, which is why the real bots do not walk like that.
pub fn route_across(area: &dyn AreaRuntime, from: Pos, exit: &Exit) -> Option<Vec<Pos>> {
    const CELL: f32 = 30.0;
    let exits = area.exits();
    // `None`: the step crosses the line of an exit that leads elsewhere; `Some(true)`: it crosses
    // one that leads where we want to go.
    let crossing = |a: Pos, b: Pos| -> Option<bool> {
        let mut crossed_any = false;
        for e in exits.iter().filter(|e| e.is_crossed_by(a, b)) {
            if e.target != exit.target {
                return None;
            }
            crossed_any = true;
        }
        Some(crossed_any)
    };
    let bounds = area.bounds();
    let cell_of = |p: Pos| (((p.x - bounds.min.x) / CELL).round() as i64, ((p.y - bounds.min.y) / CELL).round() as i64);
    // Every cell reached: the cell it was reached from, and where the walker stands in it.
    type Cell = (i64, i64);
    let mut seen: HashMap<Cell, (Option<Cell>, Pos)> = HashMap::new();
    let start = cell_of(from);
    seen.insert(start, (None, from));
    let mut queue = VecDeque::from([start]);
    while let Some(cell) = queue.pop_front() {
        let cur = seen[&cell].1;
        for (dx, dy) in [(1, 0), (-1, 0), (0, 1), (0, -1), (1, 1), (1, -1), (-1, 1), (-1, -1)] {
            let next = (cell.0 + dx, cell.1 + dy);
            if seen.contains_key(&next) {
                continue;
            }
            let want = Pos::new(bounds.min.x + next.0 as f32 * CELL, bounds.min.y + next.1 as f32 * CELL, cur.z);
            let MoveResult::Accepted(p) = area.validate_move(cur, want) else { continue };
            if !p.is_finite() || p.dist_xy(want) > 0.75 {
                continue;
            }
            let Some(crossed) = crossing(cur, p) else { continue };
            seen.insert(next, (Some(cell), p));
            if crossed {
                let mut route = vec![p];
                let mut at = cell;
                while let Some(parent) = seen[&at].0 {
                    route.push(seen[&at].1);
                    at = parent;
                }
                route.reverse();
                return Some(route);
            }
            queue.push_back(next);
        }
    }
    None
}
