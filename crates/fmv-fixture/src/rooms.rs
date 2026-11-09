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
//! With [`RoomsAdapter::with_an_unreviewed_yard`] the rooms are scripted as with `with_gates`, but the yard is one its
//! adapter has not reviewed (as an FF7 field outside the adapter's list of reviewed fields): its entry point is
//! left off, and [`AreaRuntime::story_withheld`] says so, to exercise what a server does with an area whose story the
//! adapter keeps off.
//!
//! With [`RoomsAdapter::with_story`] the rooms also have a synthetic story, to exercise phasing
//! without owned data: progression is one byte string (empty for a new account, [`STORY_ADVANCED`]
//! after the hall's story region [`STORY_REGION`] was entered), both rooms are in the empty phase for
//! the first and in [`STORY_ADVANCED_PHASE`] for the second, and the yard's guard exists only in the
//! second phase (the cast is decided by the phase).

use fmv_adapter_api::{
    AdapterError, AreaInfo, AreaKey, AreaRuntime, Bounds, Exit, ExitKind, ExitState, GameAdapter, Link, MoveResult, NpcAppearance, NpcSpawn,
    StoryPoint, StoryStep, StoryTrigger, StoryWithheld,
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

/// With [`RoomsAdapter::with_interactions`], x of the hall's walk-on link A (a line from y = -50 to 50; it fires when
/// a player walks up to it) and where it leads: 20 units from the line of link B, so a player put there has B disarmed.
pub const LINK_A_X: f32 = -150.0;
pub const LINK_A_TO: (f32, f32) = (150.0, 0.0);
/// With [`RoomsAdapter::with_interactions`], x of the hall's walk-on link B (a line from y = -50 to 50) and where it
/// leads: 20 units from the line of link A.
pub const LINK_B_X: f32 = 130.0;
pub const LINK_B_TO: (f32, f32) = (-170.0, 0.0);
/// With [`RoomsAdapter::with_interactions`], y of the hall's OK link C (a line from x = -100 to 100: a player
/// that touches it and presses OK is put at [`LINK_C_TO`]).
pub const LINK_C_Y: f32 = 100.0;
pub const LINK_C_TO: (f32, f32) = (0.0, -150.0);
/// With [`RoomsAdapter::with_interactions`], the corners of the hall's press region: OK pressed inside it takes a new
/// account to [`STORY_ADVANCED`] (point 1; walking through it does nothing).
pub const PRESS_REGION: [(f32, f32); 4] = [(150.0, 100.0), (250.0, 100.0), (250.0, 160.0), (150.0, 160.0)];
/// With [`RoomsAdapter::with_interactions`], the corners of the hall's toggle region (point 4): OK pressed inside it takes an
/// account that has advanced back to a new account's story, a step that is NOT forward (the lift's ride back), and does
/// nothing for a new account.
pub const TOGGLE_REGION: [(f32, f32); 4] = [(150.0, 170.0), (250.0, 170.0), (250.0, 230.0), (150.0, 230.0)];
/// With [`RoomsAdapter::with_interactions`], the corners of the hall's battle region (point 2): coming to it starts
/// the battle [`BATTLE_FORMATION`], and a WON battle takes a new account to [`STORY_ADVANCED`].
pub const BATTLE_REGION: [(f32, f32); 4] = [(-300.0, 200.0), (-250.0, 200.0), (-250.0, 250.0), (-300.0, 250.0)];
/// With [`RoomsAdapter::with_interactions`], the formation the battle region starts.
pub const BATTLE_FORMATION: u16 = 0;
/// With [`RoomsAdapter::with_interactions`], the corners of the hall's other battle region (point 3), which starts
/// [`LOSING_FORMATION`]: an enemy the synthetic rules make too strong to beat, so that a test can lose the fight.
pub const LOSING_REGION: [(f32, f32); 4] = [(250.0, -250.0), (300.0, -250.0), (300.0, -200.0), (250.0, -200.0)];
/// With [`RoomsAdapter::with_interactions`], the formation of the battle region the party cannot win.
pub const LOSING_FORMATION: u16 = 2;
/// With [`RoomsAdapter::with_interactions`], how far from the yard's guard (at [`YARD_NPC_POS`]) a player has to be to
/// talk to it: pressing OK next to it takes a new account to [`STORY_ADVANCED`] (point 0 of the yard).
pub const TALK_RADIUS: f32 = 80.0;

/// With [`RoomsAdapter::with_a_countdown`], the progression of an account whose countdown story has moved on: the
/// step of the hall's clear region ([`COUNTDOWN_CLEAR_REGION`]), which makes the countdown the first step started moot.
pub const STORY_CLEARED: &[u8] = &[2];
/// With [`RoomsAdapter::with_a_countdown`], how long the countdown is that the hall's story region starts (seconds).
pub const COUNTDOWN_SECS: u32 = 90;
/// With [`RoomsAdapter::with_a_countdown`], the corners of the hall's clear region (point 1): coming to it takes an
/// account that has advanced from [`STORY_ADVANCED`] to [`STORY_CLEARED`], and that ends the countdown the story region
/// started (`AreaRuntime::countdown_live` is `false` for any story but the one the countdown started in).
pub const COUNTDOWN_CLEAR_REGION: [(f32, f32); 4] = [(-300.0, 100.0), (-200.0, 100.0), (-200.0, 200.0), (-300.0, 200.0)];

/// The label of the yard's entry point (the point of a scripted yard that fires on entry).
const YARD_ENTRY_LABEL: &str = "yard entry: progress 0 -> 1";

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
    unreviewed_yard: bool,
    interactive: bool,
    countdown: Option<u32>,
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

    /// The synthetic story (as [`RoomsAdapter::with_story`]) with the things that wait for the OK button and the
    /// lines that move a player inside an area: the hall has two walk-on links ([`LINK_A_X`], [`LINK_B_X`]), one that
    /// needs OK ([`LINK_C_Y`]), a press region ([`PRESS_REGION`]) and a battle region ([`BATTLE_REGION`]); the yard's
    /// guard can be talked to ([`TALK_RADIUS`]). Every one of the story points takes a new account to
    /// [`STORY_ADVANCED`].
    pub fn with_interactions() -> Self {
        Self { story: true, interactive: true, ..Self::default() }
    }

    /// The synthetic story with a COUNTDOWN: the hall's story region ([`STORY_REGION`]) starts one of [`COUNTDOWN_SECS`]
    /// seconds with its step (`StoryStep::countdown`), and the hall's clear region ([`COUNTDOWN_CLEAR_REGION`]) takes an
    /// advanced account on to [`STORY_CLEARED`], which makes the countdown moot (`AreaRuntime::countdown_live` is only
    /// true while the account's story is still [`STORY_ADVANCED`], the one the step left).
    pub fn with_a_countdown() -> Self {
        Self::with_a_countdown_of(COUNTDOWN_SECS)
    }

    /// [`RoomsAdapter::with_a_countdown`] with a countdown of `seconds` (a test that runs the server's own clock through
    /// one uses a short one).
    pub fn with_a_countdown_of(seconds: u32) -> Self {
        Self { story: true, countdown: Some(seconds), ..Self::default() }
    }

    /// As [`RoomsAdapter::with_gates`], but the yard is a room the adapter has not reviewed: its entry point is not
    /// offered (and does not advance anybody's story), and [`AreaRuntime::story_withheld`] names it, together with
    /// one script exit the yard would have had.
    pub fn with_an_unreviewed_yard() -> Self {
        Self { story: true, gates: true, unreviewed_yard: true, ..Self::default() }
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
                withheld: self.unreviewed_yard && key == YARD,
                interactive: self.interactive,
                countdown: self.countdown.filter(|_| key == HALL),
            })),
            GHOST if self.ghost => Err(AdapterError::Decode("the ghost area cannot be loaded (by design)".into())),
            other => Err(AdapterError::AreaNotFound(other.to_string())),
        }
    }

    /// The synthetic story keeps the longer of two progressions (the advanced one over a new
    /// account's); without a story the newest write wins, as the trait's default says.
    fn merge_progression(&self, stored: &[u8], advanced: &[u8]) -> Vec<u8> {
        if self.countdown.is_some() {
            // the story with the countdown has a third state of the same length: the later byte wins
            return if (stored.len(), stored) >= (advanced.len(), advanced) { stored.to_vec() } else { advanced.to_vec() };
        }
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
    /// The adapter was built with [`RoomsAdapter::with_an_unreviewed_yard`] and this is the yard: its story is left off.
    withheld: bool,
    /// The adapter was built with [`RoomsAdapter::with_interactions`].
    interactive: bool,
    /// The adapter was built with [`RoomsAdapter::with_a_countdown`] and this is the hall.
    countdown: Option<u32>,
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
            if self.interactive {
                return vec![StoryPoint {
                    id: 0,
                    trigger: StoryTrigger::Talk { center: Pos::new(YARD_NPC_POS.0, YARD_NPC_POS.1, 0.0), radius: TALK_RADIUS },
                    label: "yard guard talk: progress 0 -> 1".into(),
                }];
            }
            // coming to stand in the yard moves a new account's story, when the rooms are scripted
            return if self.scripted && !self.withheld {
                vec![StoryPoint { id: 0, trigger: StoryTrigger::Enter, label: YARD_ENTRY_LABEL.into() }]
            } else {
                Vec::new()
            };
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
        if self.countdown.is_some() {
            let clear = COUNTDOWN_CLEAR_REGION.iter().map(|&(x, y)| Pos::new(x, y, 0.0)).collect();
            points.push(StoryPoint { id: 1, trigger: StoryTrigger::Region(clear), label: "hall clear region: progress 1 -> 2".into() });
        }
        if self.interactive {
            let corners = |c: [(f32, f32); 4]| c.iter().map(|&(x, y)| Pos::new(x, y, 0.0)).collect::<Vec<_>>();
            points.push(StoryPoint { id: 1, trigger: StoryTrigger::Press(corners(PRESS_REGION)), label: "hall press region: progress 0 -> 1".into() });
            points.push(StoryPoint { id: 4, trigger: StoryTrigger::Press(corners(TOGGLE_REGION)), label: "hall toggle region: progress 1 -> 0".into() });
            points.push(StoryPoint { id: 2, trigger: StoryTrigger::Region(corners(BATTLE_REGION)), label: "hall battle region: progress 0 -> 1".into() });
            points.push(StoryPoint { id: 3, trigger: StoryTrigger::Region(corners(LOSING_REGION)), label: "hall losing battle region: progress 0 -> 1".into() });
        }
        points
    }

    /// The hall's ladders and jumps, with the synthetic interactions: A and B are walk-on links that lead to each other's
    /// side of the hall (each puts a player 20 units from the other's line), C waits for OK.
    fn links(&self) -> Vec<Link> {
        if !self.interactive || self.is_yard() {
            return Vec::new();
        }
        let link = |line: ((f32, f32), (f32, f32)), to: (f32, f32), needs_ok: bool, label: &str| Link {
            line: (Pos::new(line.0 .0, line.0 .1, 0.0), Pos::new(line.1 .0, line.1 .1, 0.0)),
            to: Pos::new(to.0, to.1, 0.0),
            to_hint: None,
            needs_ok,
            label: label.into(),
        };
        vec![
            link(((LINK_A_X, -50.0), (LINK_A_X, 50.0)), LINK_A_TO, false, "jump"),
            link(((LINK_B_X, -50.0), (LINK_B_X, 50.0)), LINK_B_TO, false, "jump"),
            link(((-100.0, LINK_C_Y), (100.0, LINK_C_Y)), LINK_C_TO, true, "ladder"),
        ]
    }

    /// The battle the hall's battle region starts first (the step waits for the win).
    fn story_battle(&self, point: u32) -> Option<u16> {
        if !self.interactive || self.is_yard() {
            return None;
        }
        match point {
            2 => Some(BATTLE_FORMATION),
            3 => Some(LOSING_FORMATION),
            _ => None,
        }
    }

    /// With the countdown: the story region starts it with its step, the clear region ends it with its own.
    fn countdown_live(&self, started: &[u8], now: &[u8]) -> bool {
        self.countdown.is_none() || started == now
    }

    fn story_advance(&self, progress: &[u8], point: u32) -> Option<StoryStep> {
        if let Some(seconds) = self.countdown {
            return match point {
                0 => progress.is_empty().then(|| StoryStep { progress: STORY_ADVANCED.to_vec(), summary: "moment=0->1".into(), countdown: Some(seconds) }),
                1 => (progress == STORY_ADVANCED).then(|| StoryStep { progress: STORY_CLEARED.to_vec(), summary: "moment=1->2".into(), countdown: None }),
                _ => None,
            };
        }
        // The toggle region: a step that is not forward, from the advanced story back to a new account's.
        if self.interactive && !self.is_yard() && point == 4 {
            return (progress == STORY_ADVANCED).then(|| StoryStep { progress: Vec::new(), summary: "moment=1->0".into(), countdown: None });
        }
        let exists = self.story
            && if self.is_yard() {
                (self.scripted && !self.withheld && point == 0) || (self.interactive && point == 0)
            } else {
                point == 0 || (self.scripted && point == 1) || (self.interactive && point <= 3)
            };
        (exists && progress.is_empty()).then(|| StoryStep { progress: STORY_ADVANCED.to_vec(), summary: "moment=0->1".into(), countdown: None })
    }

    /// The unreviewed yard's entry point and the exit it would have had, left off.
    fn story_withheld(&self) -> Option<StoryWithheld> {
        self.withheld.then(|| StoryWithheld {
            reason: "unreviewed field".into(),
            exits: vec!["to hall when a key is pressed".into()],
            links: Vec::new(),
            points: vec![YARD_ENTRY_LABEL.into()],
        })
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
    fn an_unreviewed_yard_offers_no_story_and_says_what_it_left_off() {
        let a = RoomsAdapter::with_an_unreviewed_yard();
        let (hall, yard) = (a.load_area(HALL).unwrap(), a.load_area(YARD).unwrap());
        // the hall is as with the gates: both of its points, nothing left off
        assert_eq!(hall.story_points().len(), 2);
        assert_eq!(hall.story_withheld(), None);
        assert_eq!(hall.story_advance(&[], 1).unwrap().progress, STORY_ADVANCED);
        // the yard offers no point, advances nobody and says what it did not take; its gateway is as ever
        assert!(yard.story_points().is_empty());
        assert_eq!(yard.story_advance(&[], 0), None);
        assert_eq!(yard.exits().len(), 1);
        let left_off = yard.story_withheld().expect("the yard left something off");
        assert_eq!((left_off.reason.as_str(), left_off.exits.len(), left_off.points), ("unreviewed field", 1, vec![YARD_ENTRY_LABEL.to_string()]));
        // its phases are untouched
        assert_eq!(yard.phase_key(STORY_ADVANCED), Some(PhaseKey(STORY_ADVANCED_PHASE.to_vec())));
        // the other variants leave nothing off
        for plain in [RoomsAdapter::with_gates(), RoomsAdapter::with_story(), RoomsAdapter::new()] {
            assert!([HALL, YARD].iter().all(|k| plain.load_area(k).unwrap().story_withheld().is_none()));
        }
        assert_eq!(RoomsAdapter::with_gates().load_area(YARD).unwrap().story_points().len(), 1);
    }

    #[test]
    fn the_countdown_story_starts_a_timer_with_its_first_step_and_ends_it_with_the_second() {
        let a = RoomsAdapter::with_a_countdown();
        let (hall, yard) = (a.load_area(HALL).unwrap(), a.load_area(YARD).unwrap());
        assert_eq!(hall.story_points().iter().map(|p| p.id).collect::<Vec<_>>(), [0, 1]);
        assert!(yard.story_points().is_empty());
        // the story region's step starts the countdown, the clear region's does not
        let first = hall.story_advance(&[], 0).expect("the first step");
        assert_eq!((first.progress.as_slice(), first.countdown), (STORY_ADVANCED, Some(COUNTDOWN_SECS)));
        assert_eq!(hall.story_advance(STORY_ADVANCED, 0), None, "once");
        let second = hall.story_advance(STORY_ADVANCED, 1).expect("the second step");
        assert_eq!((second.progress.as_slice(), second.countdown), (STORY_CLEARED, None));
        assert_eq!((hall.story_advance(&[], 1), hall.story_advance(STORY_CLEARED, 1)), (None, None));
        // the countdown matters while the story is still where the first step left it
        assert!(hall.countdown_live(STORY_ADVANCED, STORY_ADVANCED));
        assert!(!hall.countdown_live(STORY_ADVANCED, STORY_CLEARED));
        assert!(!hall.countdown_live(STORY_ADVANCED, &[]));
        // the region is in the hall, away from the others, and walking into it is a hit
        let clear = &hall.story_points()[1];
        assert!(clear.is_hit_by(Pos::new(-150.0, 150.0, 0.0), Pos::new(-250.0, 150.0, 0.0)));
        assert!(!clear.is_hit_by(Pos::new(100.0, 0.0, 0.0), Pos::new(200.0, 0.0, 0.0)));
        // the later byte wins a merge, whichever order the saves arrive in
        assert_eq!(a.merge_progression(STORY_ADVANCED, STORY_CLEARED), STORY_CLEARED);
        assert_eq!(a.merge_progression(STORY_CLEARED, STORY_ADVANCED), STORY_CLEARED);
        assert_eq!(a.merge_progression(&[], STORY_ADVANCED), STORY_ADVANCED);
        // the other variants have no countdown: a step starts none and every story is live
        for plain in [RoomsAdapter::with_story(), RoomsAdapter::with_gates(), RoomsAdapter::with_interactions()] {
            let hall = plain.load_area(HALL).unwrap();
            assert_eq!(hall.story_advance(&[], 0).map(|s| s.countdown), Some(None));
            assert!(hall.countdown_live(STORY_ADVANCED, STORY_CLEARED));
        }
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
