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
//!
//! With [`RoomsAdapter::with_gates`] the hall also has exits that its "scripts" decide, like FF7's: one line
//! ([`GATE_LINE_Y`]) with three exits to the yard, one that cannot be judged ([`GATE_UNKNOWN_ARRIVAL`]), one that is
//! open only for the advanced story ([`GATE_ADVANCED_ARRIVAL`]) and one only for a new account
//! ([`GATE_NEW_ARRIVAL`]), and a line on the west wall ([`GATE_WALL_X`]) whose only exit is never open. It has the
//! synthetic story too, with two more ways to advance it: walking across a line of the hall ([`STORY_LINE_Y`]) and
//! coming to stand in the yard (a point that fires on entry).
//!
//! With [`RoomsAdapter::with_story`] the rooms also have a synthetic story, to exercise phasing
//! without owned data: progression is one byte string (empty for a new account, [`STORY_ADVANCED`]
//! after the hall's story region [`STORY_REGION`] was entered), both rooms are in the empty phase for
//! the first and in [`STORY_ADVANCED_PHASE`] for the second, and the yard's guard exists only in the
//! second phase (the cast is decided by the phase).

use fmv_adapter_api::{
    AdapterError, AreaInfo, AreaKey, AreaRuntime, Bounds, Exit, ExitKind, ExitState, GameAdapter, MoveResult, NpcAppearance, NpcSpawn,
    StoryPoint, StoryStep, StoryTrigger,
};
use fmv_core::{AreaId, EditionProvenance, Fingerprint, GameId, PhaseKey, Platform, Pos, SourceRef};

/// With [`RoomsAdapter::with_story`], the progression of an account that has advanced the synthetic
/// story (a new account's is the empty blob). Any other blob is not this story's.
pub const STORY_ADVANCED: &[u8] = &[1];
/// With [`RoomsAdapter::with_story`], the phase key of both rooms for [`STORY_ADVANCED`] (a new
/// account's is the empty key, as the adapter API requires).
pub const STORY_ADVANCED_PHASE: &[u8] = &[0xA1];
/// With [`RoomsAdapter::with_story`], the corners of the hall's story region: stepping into it takes an
/// account from the empty progression to [`STORY_ADVANCED`].
pub const STORY_REGION: [(f32, f32); 4] = [(150.0, -50.0), (250.0, -50.0), (250.0, 50.0), (150.0, 50.0)];

/// With [`RoomsAdapter::with_gates`], y of the hall's line with three exits to the yard (it runs from x = -100 to 100).
pub const GATE_LINE_Y: f32 = -250.0;
/// With [`RoomsAdapter::with_gates`], where the exit of [`GATE_LINE_Y`] that cannot be judged leads (in the yard).
pub const GATE_UNKNOWN_ARRIVAL: (f32, f32) = (-250.0, -50.0);
/// With [`RoomsAdapter::with_gates`], where the exit of [`GATE_LINE_Y`] that is open for the advanced story leads.
pub const GATE_ADVANCED_ARRIVAL: (f32, f32) = (-250.0, 100.0);
/// With [`RoomsAdapter::with_gates`], where the exit of [`GATE_LINE_Y`] that is open for a new account leads.
pub const GATE_NEW_ARRIVAL: (f32, f32) = (-250.0, -100.0);
/// With [`RoomsAdapter::with_gates`], x of the hall's line on the west wall (it runs from y = -50 to 50); its one
/// exit is never open.
pub const GATE_WALL_X: f32 = -300.0;

/// With [`RoomsAdapter::with_gates`], y of the hall's story line (it runs from x = -100 to 100): walking across it
/// takes a new account to [`STORY_ADVANCED`], like the story region.
pub const STORY_LINE_Y: f32 = 200.0;

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
    story: bool,
    gates: bool,
}

impl RoomsAdapter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Also list [`GHOST`] and give the hall an exit to it; `load_area(GHOST)` always fails.
    pub fn with_unloadable_area() -> Self {
        Self { ghost: true, ..Self::default() }
    }

    /// Give the rooms the synthetic story described in the module docs.
    pub fn with_story() -> Self {
        Self { story: true, ..Self::default() }
    }

    /// Give the hall the exits its "scripts" decide (see the module docs), with the synthetic story.
    pub fn with_gates() -> Self {
        Self { story: true, gates: true, ..Self::default() }
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
            HALL | YARD => Ok(Box::new(Room {
                info: info_for(key),
                ghost_exit: self.ghost && key == HALL,
                story: self.story,
                gates: self.gates && key == HALL,
                scripted: self.gates,
            })),
            GHOST if self.ghost => Err(AdapterError::Decode("the ghost area cannot be loaded (by design)".into())),
            other => Err(AdapterError::AreaNotFound(other.to_string())),
        }
    }

    /// The synthetic story keeps the longer of two progressions (the advanced one over a new
    /// account's); without a story the newest write wins, as the trait's default says.
    fn merge_progression(&self, stored: &[u8], advanced: &[u8]) -> Vec<u8> {
        if self.story && stored.len() >= advanced.len() {
            stored.to_vec()
        } else {
            advanced.to_vec()
        }
    }
}

/// One room: the hall or the yard.
struct Room {
    info: AreaInfo,
    /// The hall's extra exit to the unloadable area.
    ghost_exit: bool,
    /// The adapter was built with the synthetic story.
    story: bool,
    /// The hall's exits that scripts decide.
    gates: bool,
    /// The adapter was built with [`RoomsAdapter::with_gates`]: the hall has a story line and the yard a point that
    /// fires on entry.
    scripted: bool,
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
            if self.gates {
                let gated = |id: u32, line: ((f32, f32), (f32, f32)), arrival: (f32, f32)| Exit { kind: ExitKind::Script { id }, ..exit(line, YARD, arrival) };
                let line = ((-100.0, GATE_LINE_Y), (100.0, GATE_LINE_Y));
                exits.push(gated(0, line, GATE_UNKNOWN_ARRIVAL));
                exits.push(gated(1, line, GATE_ADVANCED_ARRIVAL));
                exits.push(gated(2, line, GATE_NEW_ARRIVAL));
                exits.push(gated(3, ((GATE_WALL_X, -50.0), (GATE_WALL_X, 50.0)), (-250.0, 0.0)));
            }
            exits
        }
    }

    /// The hall's exits that scripts decide: the first cannot be judged, the second is open for the advanced story
    /// only, the third for a new account only, the fourth never.
    fn exit_state(&self, exit: &Exit, progress: &[u8]) -> ExitState {
        let open_when = |yes: bool| if yes { ExitState::Open } else { ExitState::Closed };
        match exit.kind {
            ExitKind::Script { id: 0 } => ExitState::Unknown("a key the fixture's hall wants pressed".to_string()),
            ExitKind::Script { id: 1 } => open_when(progress == STORY_ADVANCED),
            ExitKind::Script { id: 2 } => open_when(progress.is_empty()),
            ExitKind::Script { .. } => ExitState::Closed,
            _ => ExitState::Open,
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

    fn phase_key(&self, progress: &[u8]) -> Option<PhaseKey> {
        if !self.story {
            return None;
        }
        match progress {
            [] => Some(PhaseKey::default()),
            p if p == STORY_ADVANCED => Some(PhaseKey(STORY_ADVANCED_PHASE.to_vec())),
            _ => None,
        }
    }

    fn story_points(&self) -> Vec<StoryPoint> {
        if !self.story {
            return Vec::new();
        }
        if self.is_yard() {
            // coming to stand in the yard moves a new account's story, when the rooms are scripted
            return if self.scripted { vec![StoryPoint { id: 0, trigger: StoryTrigger::Enter, label: "yard entry: progress 0 -> 1".into() }] } else { Vec::new() };
        }
        let mut points = vec![StoryPoint {
            id: 0,
            trigger: StoryTrigger::Region(STORY_REGION.iter().map(|&(x, y)| Pos::new(x, y, 0.0)).collect()),
            label: "hall story region: progress 0 -> 1".into(),
        }];
        if self.scripted {
            points.push(StoryPoint {
                id: 1,
                trigger: StoryTrigger::Line(Pos::new(-100.0, STORY_LINE_Y, 0.0), Pos::new(100.0, STORY_LINE_Y, 0.0)),
                label: "hall story line: progress 0 -> 1".into(),
            });
        }
        points
    }

    fn story_advance(&self, progress: &[u8], point: u32) -> Option<StoryStep> {
        let exists = self.story && if self.is_yard() { self.scripted && point == 0 } else { point == 0 || (self.scripted && point == 1) };
        (exists && progress.is_empty()).then(|| StoryStep { progress: STORY_ADVANCED.to_vec(), summary: "moment=0->1".into() })
    }

    /// The yard's guard stands only in the advanced phase.
    fn npcs_in_phase(&self, phase: &PhaseKey) -> Vec<NpcSpawn> {
        if self.story && phase.0 != STORY_ADVANCED_PHASE {
            return Vec::new();
        }
        self.npcs()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_synthetic_story_phases_advances_and_decides_the_cast() {
        // Without the story the rooms have none of it.
        let plain = RoomsAdapter::new().load_area(HALL).unwrap();
        assert!(plain.story_points().is_empty() && plain.phase_key(&[]).is_none() && plain.story_advance(&[], 0).is_none());
        assert_eq!(RoomsAdapter::new().merge_progression(&[1], &[]), Vec::<u8>::new(), "the default is the newest write");

        let a = RoomsAdapter::with_story();
        let (hall, yard) = (a.load_area(HALL).unwrap(), a.load_area(YARD).unwrap());
        for room in [&hall, &yard] {
            assert_eq!(room.phase_key(&[]), Some(PhaseKey::default()), "a new account is the empty phase");
            assert_eq!(room.phase_key(STORY_ADVANCED), Some(PhaseKey(STORY_ADVANCED_PHASE.to_vec())));
            assert_eq!(room.phase_key(&[7, 7]), None, "not this story's blob");
        }
        // One story point, in the hall, that only a new account can use.
        assert!(yard.story_points().is_empty());
        let points = hall.story_points();
        assert_eq!(points.len(), 1);
        assert!(points[0].is_hit_by(Pos::new(100.0, 0.0, 0.0), Pos::new(200.0, 0.0, 0.0)));
        assert!(!points[0].is_hit_by(Pos::new(-100.0, 0.0, 0.0), Pos::new(100.0, 0.0, 0.0)), "the spawns are outside it");
        assert_eq!(hall.story_advance(&[], 0).unwrap().progress, STORY_ADVANCED);
        assert_eq!(hall.story_advance(STORY_ADVANCED, 0), None);
        assert_eq!(hall.story_advance(&[], 1), None);
        assert_eq!(yard.story_advance(&[], 0), None);
        // The cast is decided by the phase: the guard is only in the advanced one.
        assert!(yard.npcs_in_phase(&PhaseKey::default()).is_empty());
        assert_eq!(yard.npcs_in_phase(&PhaseKey(STORY_ADVANCED_PHASE.to_vec())).len(), 1);
        // Merging keeps the advanced story, whichever order the saves arrive in.
        assert_eq!(a.merge_progression(&[], STORY_ADVANCED), STORY_ADVANCED);
        assert_eq!(a.merge_progression(STORY_ADVANCED, &[]), STORY_ADVANCED);
    }

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
    fn the_gated_hall_has_a_shared_line_whose_open_exit_depends_on_the_story_and_a_wall_line_that_never_opens() {
        let hall = RoomsAdapter::with_gates().load_area(HALL).unwrap();
        let exits = hall.exits();
        assert_eq!(exits.len(), 5, "the gateway to the yard, three exits on one line, one on the wall");
        assert_eq!(exits[0].kind, ExitKind::Gateway);
        assert!(exits[1..4].iter().all(|e| e.same_line_as(&exits[1]) && e.target == YARD));
        assert!(!exits[4].same_line_as(&exits[1]));
        let ids: Vec<ExitKind> = exits[1..].iter().map(|e| e.kind).collect();
        assert_eq!(ids, [ExitKind::Script { id: 0 }, ExitKind::Script { id: 1 }, ExitKind::Script { id: 2 }, ExitKind::Script { id: 3 }]);
        let states = |progress: &[u8]| exits.iter().map(|e| hall.exit_state(e, progress)).collect::<Vec<_>>();
        let unknown = ExitState::Unknown("a key the fixture's hall wants pressed".to_string());
        assert_eq!(states(&[]), [ExitState::Open, unknown.clone(), ExitState::Closed, ExitState::Open, ExitState::Closed]);
        assert_eq!(states(STORY_ADVANCED), [ExitState::Open, unknown, ExitState::Open, ExitState::Closed, ExitState::Closed]);
        // the plain rooms and the yard have no script exits at all
        assert_eq!(RoomsAdapter::with_story().load_area(HALL).unwrap().exits().len(), 1);
        assert_eq!(RoomsAdapter::with_gates().load_area(YARD).unwrap().exits().len(), 1);
        // the exits walk the line from either side
        assert!(exits[1].is_crossed_by(Pos::new(0.0, -200.0, 0.0), Pos::new(0.0, -280.0, 0.0)));
        assert!(exits[4].is_crossed_by(Pos::new(-250.0, 0.0, 0.0), Pos::new(-350.0, 0.0, 0.0)));
    }

    #[test]
    fn the_scripted_rooms_move_a_new_accounts_story_on_a_line_and_on_entry() {
        let a = RoomsAdapter::with_gates();
        let (hall, yard) = (a.load_area(HALL).unwrap(), a.load_area(YARD).unwrap());
        let points = hall.story_points();
        assert_eq!(points.len(), 2);
        assert!(points[1].is_hit_by(Pos::new(0.0, 150.0, 0.0), Pos::new(0.0, 250.0, 0.0)));
        assert!(!points[1].is_hit_by(Pos::new(0.0, 150.0, 0.0), Pos::new(0.0, 190.0, 0.0)));
        assert_eq!(hall.story_advance(&[], 1).unwrap().progress, STORY_ADVANCED);
        assert_eq!(hall.story_advance(STORY_ADVANCED, 1), None);
        let entry = yard.story_points();
        assert_eq!(entry.len(), 1);
        assert!(entry[0].is_on_entry() && !entry[0].is_hit_by(Pos::default(), Pos::new(100.0, 100.0, 0.0)));
        assert_eq!(yard.story_advance(&[], 0).unwrap().progress, STORY_ADVANCED);
        assert_eq!(yard.story_advance(STORY_ADVANCED, 0), None);
        // without the scripts the yard has no point and the hall only its region
        let story = RoomsAdapter::with_story();
        assert!(story.load_area(YARD).unwrap().story_points().is_empty());
        assert_eq!(story.load_area(HALL).unwrap().story_points().len(), 1);
        assert_eq!(story.load_area(HALL).unwrap().story_advance(&[], 1), None);
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
