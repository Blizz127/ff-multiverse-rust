//! A FIXTURE adapter with several connected areas, for exercising multi-area play (exits,
//! travel, NPCs, per-area rules) without owned FF7 data.
//!
//! **Synthetic test content, NOT FF7 and NOT evidence of FF7 support**: every area of it is
//! labelled FIXTURE like the plaza of [`crate::FixtureAdapter`].
//!
//! ```text
//!   hall  (x -400..400, y -300..300, max speed 600)       yard  (same size, max speed 1200)
//!   +--------------------------------+                    +--------------------------------+
//!   |                           |    |   exit at x = 300  |    |                           |
//!   |   spawns (-100, 0), (100, 0)  -----> arrives (-200, 0)  <-----  exit at x = -300      |
//!   |                           |    |   <-- arrives (200, 0) |   a guard at (200, 100)    |
//!   +--------------------------------+                    +--------------------------------+
//! ```
//!
//! `hall` leads to `yard` across the line x = 300 and `yard` leads back across x = -300; the
//! arrival points are 100 units inside the target, away from the line that leads back. The yard
//! is faster than the hall (it plays the part of a field of a larger scale) and has one NPC. With
//! [`RoomsAdapter::with_unloadable_area`] the hall also has an exit at its top edge (y = 250) to
//! `ghost`, an area the adapter lists but cannot load, to test what a server does with a target
//! that turns out to be unusable.

use fmv_adapter_api::{
    AdapterError, AreaInfo, AreaKey, AreaRuntime, Bounds, Exit, ExitKind, GameAdapter, MoveResult, NpcAppearance, NpcSpawn,
};
use fmv_core::{AreaId, EditionProvenance, Fingerprint, GameId, Platform, Pos, SourceRef};

/// The area the players enter first.
pub const HALL: &str = "hall";
/// The area behind the hall's exit.
pub const YARD: &str = "yard";
/// Listed (with [`RoomsAdapter::with_unloadable_area`]) but never loadable.
pub const GHOST: &str = "ghost";

/// x of the hall's exit line (the yard's is its negative).
pub const EXIT_X: f32 = 300.0;
/// Where an entity arrives in the yard (from the hall) and in the hall (from the yard).
pub const YARD_ARRIVAL: (f32, f32) = (-200.0, 0.0);
pub const HALL_ARRIVAL: (f32, f32) = (200.0, 0.0);
/// The yard's one NPC.
pub const YARD_NPC_POS: (f32, f32) = (200.0, 100.0);
pub const HALL_MAX_SPEED: f32 = 600.0;
pub const YARD_MAX_SPEED: f32 = 1200.0;

/// A rectangle of two triangles.
const RECT: [[(f32, f32); 3]; 2] = [
    [(-400.0, -300.0), (400.0, -300.0), (400.0, 300.0)],
    [(-400.0, -300.0), (400.0, 300.0), (-400.0, 300.0)],
];

fn inside(t: &[(f32, f32); 3], p: (f32, f32)) -> bool {
    let s = |a: (f32, f32), b: (f32, f32), c: (f32, f32)| (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0);
    let d1 = s(t[0], t[1], p);
    let d2 = s(t[1], t[2], p);
    let d3 = s(t[2], t[0], p);
    let neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(neg && pos)
}

fn on_mesh(p: (f32, f32)) -> bool {
    RECT.iter().any(|t| inside(t, p))
}

fn info_for(key: &str) -> AreaInfo {
    let mut h = blake3::Hasher::new();
    h.update(b"fmv-fixture/rooms/v1/");
    h.update(key.as_bytes());
    AreaInfo {
        id: AreaId { game: GameId::Fixture, key: key.to_string() },
        display_name: format!("FIXTURE {key} (synthetic, not FF7)"),
        fingerprint: Fingerprint(*h.finalize().as_bytes()),
        provenance: EditionProvenance {
            game: GameId::Fixture,
            platform: Platform::Synthetic,
            edition: "fixture-rooms".into(),
            region: "none".into(),
            language: "none".into(),
            revision: "v1".into(),
            sources: vec![SourceRef::Fixture],
            modded: false,
        },
        fixture: true,
    }
}

/// A fixture adapter with the areas [`HALL`] and [`YARD`] (see the module docs).
#[derive(Debug, Clone, Default)]
pub struct RoomsAdapter {
    ghost: bool,
}

impl RoomsAdapter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Also list [`GHOST`] and give the hall an exit to it; `load_area(GHOST)` always fails.
    pub fn with_unloadable_area() -> Self {
        Self { ghost: true }
    }

    fn keys(&self) -> Vec<&'static str> {
        if self.ghost {
            vec![HALL, YARD, GHOST]
        } else {
            vec![HALL, YARD]
        }
    }
}

impl GameAdapter for RoomsAdapter {
    fn game(&self) -> GameId {
        GameId::Fixture
    }

    fn list_areas(&self) -> Vec<AreaInfo> {
        self.keys().into_iter().map(info_for).collect()
    }

    fn load_area(&self, key: &str) -> Result<Box<dyn AreaRuntime>, AdapterError> {
        match key {
            HALL | YARD => Ok(Box::new(Room { info: info_for(key), ghost_exit: self.ghost && key == HALL })),
            GHOST if self.ghost => Err(AdapterError::Decode("the ghost area cannot be loaded (by design)".into())),
            other => Err(AdapterError::AreaNotFound(other.to_string())),
        }
    }
}

/// One room: the hall or the yard.
struct Room {
    info: AreaInfo,
    /// The hall's extra exit to the unloadable area.
    ghost_exit: bool,
}

impl Room {
    fn is_yard(&self) -> bool {
        self.info.id.key == YARD
    }
}

impl AreaRuntime for Room {
    fn info(&self) -> &AreaInfo {
        &self.info
    }

    fn spawn_points(&self) -> Vec<Pos> {
        vec![Pos::new(-100.0, 0.0, 0.0), Pos::new(100.0, 0.0, 0.0)]
    }

    fn validate_move(&self, from: Pos, to: Pos) -> MoveResult {
        if to.is_finite() && on_mesh((to.x, to.y)) {
            MoveResult::Accepted(Pos::new(to.x, to.y, 0.0))
        } else if from.is_finite() && on_mesh((from.x, from.y)) {
            MoveResult::Corrected(Pos::new(from.x, from.y, 0.0))
        } else {
            MoveResult::Corrected(self.spawn_points()[0])
        }
    }

    fn bounds(&self) -> Bounds {
        Bounds { min: Pos::new(-400.0, -300.0, 0.0), max: Pos::new(400.0, 300.0, 0.0) }
    }

    fn walk_triangles(&self) -> Vec<[Pos; 3]> {
        RECT.iter().map(|t| t.map(|(x, y)| Pos::new(x, y, 0.0))).collect()
    }

    fn max_speed(&self) -> f32 {
        if self.is_yard() {
            YARD_MAX_SPEED
        } else {
            HALL_MAX_SPEED
        }
    }

    fn exits(&self) -> Vec<Exit> {
        let exit = |line: ((f32, f32), (f32, f32)), target: &str, arrival: (f32, f32)| Exit {
            line: (Pos::new(line.0 .0, line.0 .1, 0.0), Pos::new(line.1 .0, line.1 .1, 0.0)),
            target: AreaKey::from(target),
            arrival: Pos::new(arrival.0, arrival.1, 0.0),
            arrival_facing: None,
            arrival_hint: None,
            kind: ExitKind::Gateway,
        };
        if self.is_yard() {
            vec![exit(((-EXIT_X, -300.0), (-EXIT_X, 300.0)), HALL, HALL_ARRIVAL)]
        } else {
            let mut exits = vec![exit(((EXIT_X, -300.0), (EXIT_X, 300.0)), YARD, YARD_ARRIVAL)];
            if self.ghost_exit {
                exits.push(exit(((-100.0, 250.0), (100.0, 250.0)), GHOST, (0.0, 0.0)));
            }
            exits
        }
    }

    fn npcs(&self) -> Vec<NpcSpawn> {
        if !self.is_yard() {
            return Vec::new();
        }
        vec![NpcSpawn {
            entity_name: "guard".into(),
            appearance: NpcAppearance { model: "guardm".into(), idle_animation: Some("idle".into()), label: "yard guard".into() },
            pos: Pos::new(YARD_NPC_POS.0, YARD_NPC_POS.1, 0.0),
            facing: 3.0,
            unconditional: true,
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rooms_are_connected_both_ways_and_labelled_fixture() {
        let a = RoomsAdapter::new();
        let areas = a.list_areas();
        assert_eq!(areas.iter().map(|i| i.id.key.as_str()).collect::<Vec<_>>(), [HALL, YARD]);
        assert!(areas.iter().all(|i| i.fixture && i.display_name.contains("FIXTURE")));
        assert_ne!(areas[0].fingerprint, areas[1].fingerprint);
        let hall = a.load_area(HALL).unwrap();
        let yard = a.load_area(YARD).unwrap();
        let (to_yard, to_hall) = (hall.exits(), yard.exits());
        assert_eq!((to_yard.len(), to_yard[0].target.as_str()), (1, YARD));
        assert_eq!((to_hall.len(), to_hall[0].target.as_str()), (1, HALL));
        // The arrival points are on the target's floor and on the near side of the line back.
        assert_eq!(yard.place_arrival(to_yard[0].arrival, None), Pos::new(YARD_ARRIVAL.0, YARD_ARRIVAL.1, 0.0));
        assert_eq!(hall.place_arrival(to_hall[0].arrival, None), Pos::new(HALL_ARRIVAL.0, HALL_ARRIVAL.1, 0.0));
        assert!(!to_hall[0].is_crossed_by(Pos::new(-200.0, 0.0, 0.0), Pos::new(-200.0, 0.0, 0.0)));
        // Walking the hall's spawn to the line crosses it.
        assert!(to_yard[0].is_crossed_by(Pos::new(100.0, 0.0, 0.0), Pos::new(350.0, 0.0, 0.0)));
        assert!(hall.npcs().is_empty());
        let npcs = yard.npcs();
        assert_eq!((npcs.len(), npcs[0].entity_name.as_str(), npcs[0].unconditional), (1, "guard", true));
        assert_eq!((hall.max_speed(), yard.max_speed()), (HALL_MAX_SPEED, YARD_MAX_SPEED));
    }

    #[test]
    fn the_unloadable_variant_lists_an_area_that_cannot_be_loaded() {
        let a = RoomsAdapter::with_unloadable_area();
        assert_eq!(a.list_areas().len(), 3);
        assert!(a.load_area(GHOST).is_err());
        assert_eq!(a.load_area(HALL).unwrap().exits().len(), 2);
        assert!(RoomsAdapter::new().load_area(GHOST).is_err());
        assert_eq!(RoomsAdapter::new().load_area(HALL).unwrap().exits().len(), 1);
    }
}
