//! Scripted headless bot movement.
//!
//! The bot never teleports: each step moves toward a moving target at `BOT_SPEED_FACTOR *
//! max_speed` at most, and the step is validated with `AreaRuntime::validate_move`. When a step
//! is refused the pattern shrinks, so the bot converges on a path that stays on the walkable
//! surface whatever the area's shape or the spawn point. If the adapter answers `Corrected` with
//! a position other than the bot's own (it stands off the walkable surface), the bot is placed
//! there, exactly as the server will place it.

use crate::cli::BotKind;
use fmv_adapter_api::{AreaRuntime, Bounds, MoveResult};
use fmv_core::Pos;

/// Bots walk at this fraction of the area's `max_speed` (the server enforces the maximum).
pub const BOT_SPEED_FACTOR: f32 = 0.6;
/// A pattern is abandoned once it shrinks below this fraction of its initial size.
const MIN_SIZE_FRACTION: f32 = 0.02;
const SHRINK: f32 = 0.75;

#[derive(Debug, Clone)]
pub struct Bot {
    kind: BotKind,
    center: Pos,
    speed: f32,
    /// Circle radius / line half-length in area units.
    size: f32,
    min_size: f32,
    /// Circle phase (radians) or line direction angle.
    phase: f32,
    /// Line only: which end is the current target.
    toward_far_end: bool,
}

/// Outcome of one bot step.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BotStep {
    pub pos: Pos,
    /// Direction of travel in radians, if the bot moved.
    pub facing: Option<f32>,
    pub moved: bool,
}

impl Bot {
    pub fn new(kind: BotKind, spawn: Pos, bounds: Bounds, max_speed: f32) -> Self {
        let w = (bounds.max.x - bounds.min.x).abs();
        let h = (bounds.max.y - bounds.min.y).abs();
        let extent = w.min(h);
        let extent = if extent.is_finite() && extent > 0.0 {
            extent
        } else {
            100.0
        };
        let size = (extent * 0.25).max(1.0);
        let speed = if max_speed.is_finite() && max_speed > 0.0 {
            max_speed * BOT_SPEED_FACTOR
        } else {
            0.0
        };
        Self {
            kind,
            center: spawn,
            speed,
            size,
            min_size: size * MIN_SIZE_FRACTION,
            phase: 0.0,
            toward_far_end: true,
        }
    }

    /// True once the pattern shrank to nothing (the bot stands still from then on).
    pub fn is_stuck(&self) -> bool {
        self.size < self.min_size
    }

    fn target(&self) -> Pos {
        let c = self.center;
        match self.kind {
            BotKind::Idle => c,
            BotKind::Circle => Pos::new(
                c.x + self.size * self.phase.cos(),
                c.y + self.size * self.phase.sin(),
                c.z,
            ),
            BotKind::Line => {
                let sign = if self.toward_far_end { 1.0 } else { -1.0 };
                Pos::new(
                    c.x + sign * self.size * self.phase.cos(),
                    c.y + sign * self.size * self.phase.sin(),
                    c.z,
                )
            }
        }
    }

    /// Advance the bot by `dt` seconds from `cur`.
    pub fn step(&mut self, area: &dyn AreaRuntime, cur: Pos, dt: f32) -> BotStep {
        let still = BotStep {
            pos: cur,
            facing: None,
            moved: false,
        };
        if self.kind == BotKind::Idle || self.is_stuck() || !(dt.is_finite() && dt > 0.0) || self.speed <= 0.0 {
            return still;
        }
        if self.kind == BotKind::Circle {
            // Angular speed keeps the target's linear speed equal to the bot's speed.
            self.phase = (self.phase + self.speed / self.size * dt) % std::f32::consts::TAU;
        }
        let target = self.target();
        let (dx, dy) = (target.x - cur.x, target.y - cur.y);
        let dist = dx.hypot(dy);
        let max_step = self.speed * dt;
        if self.kind == BotKind::Line && dist <= max_step {
            self.toward_far_end = !self.toward_far_end;
        }
        let (sx, sy) = if dist <= max_step {
            (dx, dy)
        } else {
            (dx / dist * max_step, dy / dist * max_step)
        };
        if sx == 0.0 && sy == 0.0 {
            return still;
        }
        let want = Pos::new(cur.x + sx, cur.y + sy, cur.z);
        match area.validate_move(cur, want) {
            MoveResult::Accepted(p) if p.is_finite() => BotStep {
                pos: p,
                facing: Some(sy.atan2(sx)),
                moved: true,
            },
            // Refused AND told to stand elsewhere (`cur` is off the walkable surface or at the
            // wrong height): stand there, as the server will place us. The pattern leaves the
            // walkable surface as well, so it shrinks like for any refusal.
            MoveResult::Corrected(p) if p.is_finite() && p != cur => {
                self.size *= SHRINK;
                BotStep {
                    pos: p,
                    facing: None,
                    moved: false,
                }
            }
            _ => {
                // Refused: the pattern leaves the walkable surface, so make it smaller.
                self.size *= SHRINK;
                still
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmv_adapter_api::GameAdapter;
    use fmv_fixture::{FixtureAdapter, FIXTURE_AREA_KEY};

    /// Independent on-mesh check (does not use `validate_move`).
    fn on_mesh(tris: &[[Pos; 3]], p: Pos) -> bool {
        let side = |a: Pos, b: Pos, c: Pos| (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
        tris.iter().any(|t| {
            let (d1, d2, d3) = (side(t[0], t[1], p), side(t[1], t[2], p), side(t[2], t[0], p));
            let neg = d1 < -1e-3 || d2 < -1e-3 || d3 < -1e-3;
            let pos = d1 > 1e-3 || d2 > 1e-3 || d3 > 1e-3;
            !(neg && pos)
        })
    }

    struct Run {
        max_step: f32,
        path_len: f32,
        min: (f32, f32),
        max: (f32, f32),
        stuck: bool,
    }

    /// Simulate `secs` seconds at 50 ms ticks, asserting the invariants on every single step.
    fn simulate(kind: BotKind, spawn: Pos, secs: f32) -> Run {
        let adapter = FixtureAdapter::new();
        let area = adapter.load_area(FIXTURE_AREA_KEY).unwrap();
        let tris = area.walk_triangles();
        let mut bot = Bot::new(kind, spawn, area.bounds(), area.max_speed());
        let dt = 0.05;
        let limit = area.max_speed() * BOT_SPEED_FACTOR * dt * 1.001;
        let mut cur = spawn;
        let mut run = Run {
            max_step: 0.0,
            path_len: 0.0,
            min: (cur.x, cur.y),
            max: (cur.x, cur.y),
            stuck: false,
        };
        for _ in 0..(secs / dt) as usize {
            let s = bot.step(area.as_ref(), cur, dt);
            let d = cur.dist_xy(s.pos);
            assert!(d <= limit, "{kind:?}: step {d} exceeds {limit} (0.6 * max_speed * dt)");
            assert!(d < area.max_speed() * dt, "{kind:?}: step must stay below max_speed");
            assert!(
                on_mesh(&tris, s.pos),
                "{kind:?}: left the mesh at ({}, {}) from spawn {spawn:?}",
                s.pos.x,
                s.pos.y
            );
            assert_eq!(s.moved, s.facing.is_some());
            run.max_step = run.max_step.max(d);
            run.path_len += d;
            cur = s.pos;
            run.min = (run.min.0.min(cur.x), run.min.1.min(cur.y));
            run.max = (run.max.0.max(cur.x), run.max.1.max(cur.y));
        }
        run.stuck = bot.is_stuck();
        run
    }

    #[test]
    fn circle_bot_stays_on_mesh_below_max_speed_for_every_fixture_spawn() {
        let adapter = FixtureAdapter::new();
        let area = adapter.load_area(FIXTURE_AREA_KEY).unwrap();
        for spawn in area.spawn_points() {
            let r = simulate(BotKind::Circle, spawn, 30.0);
            assert!(!r.stuck, "spawn {spawn:?}");
            assert!(
                r.path_len > 500.0,
                "bot should actually travel, got {} from {spawn:?}",
                r.path_len
            );
            assert!(
                r.max.0 - r.min.0 > 50.0 && r.max.1 - r.min.1 > 50.0,
                "a circle spans both axes: {:?}..{:?}",
                r.min,
                r.max
            );
            // 0.6 * 600 * 0.05 = 18 units per tick; never faster.
            assert!(r.max_step <= 18.0 * 1.001, "max step {}", r.max_step);
        }
    }

    #[test]
    fn line_bot_stays_on_mesh_and_covers_ground() {
        let adapter = FixtureAdapter::new();
        let area = adapter.load_area(FIXTURE_AREA_KEY).unwrap();
        for spawn in area.spawn_points() {
            let r = simulate(BotKind::Line, spawn, 30.0);
            assert!(!r.stuck, "spawn {spawn:?}");
            assert!(r.path_len > 500.0, "got {} from {spawn:?}", r.path_len);
        }
    }

    #[test]
    fn bots_adapt_near_walls_and_corners() {
        // Spawns pressed against edges force the pattern to shrink; the path must stay on the mesh.
        for spawn in [
            Pos::new(695.0, -5.0, 0.0),   // far corner of the arm
            Pos::new(-399.0, 299.0, 0.0), // corner of the main quad
            Pos::new(0.0, 299.5, 0.0),    // along the top wall
            Pos::new(420.0, -295.0, 0.0), // bottom edge, in the arm
        ] {
            for kind in [BotKind::Circle, BotKind::Line] {
                let r = simulate(kind, spawn, 20.0);
                assert!(r.max_step <= 18.0 * 1.001);
            }
        }
    }

    #[test]
    fn idle_bot_never_moves() {
        let r = simulate(BotKind::Idle, Pos::new(-100.0, 0.0, 0.0), 5.0);
        assert_eq!(r.path_len, 0.0);
    }

    #[test]
    fn first_step_moves_immediately_and_reports_facing() {
        let adapter = FixtureAdapter::new();
        let area = adapter.load_area(FIXTURE_AREA_KEY).unwrap();
        let mut bot = Bot::new(
            BotKind::Circle,
            Pos::new(-100.0, 0.0, 0.0),
            area.bounds(),
            area.max_speed(),
        );
        let s = bot.step(area.as_ref(), Pos::new(-100.0, 0.0, 0.0), 0.05);
        assert!(s.moved && s.facing.is_some());
    }

    #[test]
    fn degenerate_inputs_do_not_panic() {
        let adapter = FixtureAdapter::new();
        let area = adapter.load_area(FIXTURE_AREA_KEY).unwrap();
        let spawn = Pos::new(0.0, 0.0, 0.0);
        let mut bot = Bot::new(BotKind::Circle, spawn, area.bounds(), f32::NAN);
        assert!(!bot.step(area.as_ref(), spawn, 0.05).moved);
        let mut bot = Bot::new(BotKind::Circle, spawn, area.bounds(), 600.0);
        assert!(!bot.step(area.as_ref(), spawn, f32::NAN).moved);
        assert!(!bot.step(area.as_ref(), spawn, -1.0).moved);
        let zero = Bounds {
            min: Pos::default(),
            max: Pos::default(),
        };
        let mut bot = Bot::new(BotKind::Line, spawn, zero, 600.0);
        let _ = bot.step(area.as_ref(), spawn, 0.05);
        // A spawn off the mesh cannot be walked on: the bot gives up instead of looping.
        let mut bot = Bot::new(BotKind::Circle, Pos::new(5000.0, 5000.0, 0.0), area.bounds(), 600.0);
        for _ in 0..200 {
            bot.step(area.as_ref(), Pos::new(5000.0, 5000.0, 0.0), 0.05);
        }
        assert!(bot.is_stuck());
    }

    /// `Corrected(p)` means "place the entity at p": a bot that finds itself off the walkable
    /// surface is put where the adapter says, instead of staying where it is.
    #[test]
    fn a_bot_off_the_mesh_is_placed_where_the_adapter_says() {
        let adapter = FixtureAdapter::new();
        let area = adapter.load_area(FIXTURE_AREA_KEY).unwrap();
        let spawn = Pos::new(-100.0, 0.0, 0.0);
        let mut bot = Bot::new(BotKind::Circle, spawn, area.bounds(), area.max_speed());
        let off = Pos::new(5000.0, 5000.0, 0.0);
        let placed = bot.step(area.as_ref(), off, 0.05);
        assert_eq!(placed.pos, area.spawn_points()[0]);
        assert!(!placed.moved && placed.facing.is_none(), "being placed is not walking");
        // From there it walks normally again.
        let next = bot.step(area.as_ref(), placed.pos, 0.05);
        assert!(next.moved && next.facing.is_some());
        assert!(placed.pos.dist_xy(next.pos) <= area.max_speed() * BOT_SPEED_FACTOR * 0.05 * 1.001);
        // A wall (Corrected onto the spot the bot already stands on) still just shrinks the
        // pattern without moving the bot.
        let corner = Pos::new(-399.9, 299.9, 0.0);
        let mut bot = Bot::new(BotKind::Circle, corner, area.bounds(), area.max_speed());
        let before = bot.size;
        let s = bot.step(area.as_ref(), corner, 0.05);
        assert!(!s.moved && s.pos == corner, "the first step from this corner leaves the mesh");
        assert!(bot.size < before, "a refused step shrinks the pattern");
    }
}
