//! Tests of the navigator and the route search, on a mock area (two rooms and a loft that only links join) and on the
//! FIXTURE hall with its interactions. [`Sim`] stands in for the server: it fires the links by the same rules
//! (`LinkArming`, `Link`), so the bot is judged by what the server would do with its moves.

use super::*;
use crate::bot::TravelBot;
use fmv_adapter_api::{AreaInfo, Bounds, Exit, ExitKind, GameAdapter, StoryPoint, StoryStep, StoryTrigger};
use fmv_fixture::rooms::{RoomsAdapter, HALL, LINK_A_TO};

/// A rectangle of the surface, at one height.
pub(crate) struct Room {
    x: (f32, f32),
    y: (f32, f32),
    z: f32,
}

impl Room {
    fn holds(&self, x: f32, y: f32) -> bool {
        (self.x.0..=self.x.1).contains(&x) && (self.y.0..=self.y.1).contains(&y)
    }
}

/// The West room (x -300..-100, y -100..100), the East room (x 100..340) and the Loft over beyond the East room's north
/// wall (x 100..340, y 200..400, height 500), none of them connected on foot:
///
/// * link 0, WALK-ON: the line x = -110 (y -50..50) in the West room, to (120, 0) in the East room;
/// * link 1, needs OK: the line y = 90 (x 150..250) in the East room, to (200, 300) in the Loft;
/// * the Loft's exit: the line x = 300 (y 200..400), to `next`.
///
/// With `roomy` false the rooms end at x = 300, so that the surface ends AT the exit's line. With a story
/// ([`Floors::with_story`]) the Loft has one press point (a panel around (200, 300)) that moves a new account's story.
pub(crate) struct Floors {
    info: AreaInfo,
    rooms: Vec<Room>,
    story: bool,
}

impl Floors {
    pub(crate) fn new(roomy: bool) -> Self {
        let east = if roomy { 340.0 } else { 300.0 };
        Self {
            story: false,
            info: RoomsAdapter::new().list_areas().remove(0),
            rooms: vec![
                Room { x: (-300.0, -100.0), y: (-100.0, 100.0), z: 0.0 },
                Room { x: (100.0, east), y: (-100.0, 100.0), z: 0.0 },
                Room { x: (100.0, east), y: (200.0, 400.0), z: 500.0 },
            ],
        }
    }
}

impl Floors {
    /// The same, with the press point in the Loft.
    pub(crate) fn with_story(mut self) -> Self {
        self.story = true;
        self
    }
}

impl AreaRuntime for Floors {
    fn info(&self) -> &AreaInfo {
        &self.info
    }
    fn spawn_points(&self) -> Vec<Pos> {
        vec![Pos::new(-200.0, 0.0, 0.0)]
    }
    fn validate_move(&self, from: Pos, to: Pos) -> MoveResult {
        if !(from.is_finite() && to.is_finite()) {
            return MoveResult::Corrected(self.spawn_points()[0]);
        }
        let Some(room) = self.rooms.iter().find(|r| r.holds(from.x, from.y) && (r.z - from.z).abs() < 30.0) else {
            return MoveResult::Corrected(self.spawn_points()[0]);
        };
        if room.holds(to.x, to.y) {
            MoveResult::Accepted(Pos::new(to.x, to.y, room.z))
        } else {
            MoveResult::Corrected(Pos::new(from.x, from.y, room.z))
        }
    }
    fn bounds(&self) -> Bounds {
        Bounds { min: Pos::new(-300.0, -100.0, 0.0), max: Pos::new(340.0, 400.0, 500.0) }
    }
    fn walk_triangles(&self) -> Vec<[Pos; 3]> {
        Vec::new()
    }
    fn max_speed(&self) -> f32 {
        600.0
    }
    fn exits(&self) -> Vec<Exit> {
        vec![Exit {
            line: (Pos::new(300.0, 200.0, 500.0), Pos::new(300.0, 400.0, 500.0)),
            target: AreaKey::from("next"),
            arrival: Pos::new(0.0, 0.0, 0.0),
            arrival_facing: None,
            arrival_hint: None,
            kind: ExitKind::Gateway,
        }]
    }
    fn links(&self) -> Vec<Link> {
        vec![
            Link {
                line: (Pos::new(-110.0, -50.0, 0.0), Pos::new(-110.0, 50.0, 0.0)),
                to: Pos::new(120.0, 0.0, 0.0),
                to_hint: None,
                needs_ok: false,
                label: "jump".into(),
            },
            Link {
                line: (Pos::new(150.0, 90.0, 0.0), Pos::new(250.0, 90.0, 0.0)),
                to: Pos::new(200.0, 300.0, 0.0),
                to_hint: Some(1),
                needs_ok: true,
                label: "ladder".into(),
            },
        ]
    }
    fn place_arrival(&self, arrival: Pos, _hint: Option<u32>) -> Pos {
        self.rooms.iter().find(|r| r.holds(arrival.x, arrival.y)).map_or(arrival, |r| Pos::new(arrival.x, arrival.y, r.z))
    }
    fn story_points(&self) -> Vec<StoryPoint> {
        if !self.story {
            return Vec::new();
        }
        let corners = [(180.0, 280.0), (220.0, 280.0), (220.0, 320.0), (180.0, 320.0)];
        vec![StoryPoint {
            id: 0,
            trigger: StoryTrigger::Press(corners.iter().map(|&(x, y)| Pos::new(x, y, 500.0)).collect()),
            label: "loft panel".into(),
        }]
    }
    fn story_advance(&self, progress: &[u8], point: u32) -> Option<StoryStep> {
        (self.story && point == 0 && progress.is_empty()).then(|| StoryStep { progress: vec![1], summary: "moment=1->2".into() })
    }
}

/// The server's side of the links, for one walker: fires a walk-on link when a move touches it while armed, an OK link
/// on a press at its foot, and puts the walker at the destination with the arming starting over.
pub(crate) struct Sim<'a> {
    pub(crate) area: &'a dyn AreaRuntime,
    pub(crate) links: Vec<Link>,
    pub(crate) pos: Pos,
    pub(crate) arming: LinkArming,
    /// The links fired, in order.
    pub(crate) fired: Vec<usize>,
    /// Walk-on links the server does not fire at the first touch, nor again until the walker has been out of their
    /// reach (it did not agree that they were armed).
    pub(crate) numb: BTreeSet<usize>,
    /// The ones it is not firing now.
    swallowing: BTreeSet<usize>,
    /// The next this many presses reach nothing (and are counted).
    pub(crate) deaf_press: u32,
    pub(crate) presses: u32,
}

impl<'a> Sim<'a> {
    pub(crate) fn new(area: &'a dyn AreaRuntime, pos: Pos) -> Self {
        let links = area.links();
        let arming = LinkArming::placed_at(&links, pos);
        Self { area, links, pos, arming, fired: Vec::new(), numb: BTreeSet::new(), swallowing: BTreeSet::new(), deaf_press: 0, presses: 0 }
    }

    /// The walker made `step` (the intent the server judges).
    pub(crate) fn apply(&mut self, step: BotStep) {
        let from = self.pos;
        self.pos = step.pos;
        if let Some(i) = self.arming.step(&self.links, from, step.pos) {
            if self.numb.remove(&i) {
                self.swallowing.insert(i);
            }
            if !self.swallowing.contains(&i) {
                self.relocate(i);
                return;
            }
        }
        // out of reach of a line the server did not fire: it is armed for it from now on
        let links = &self.links;
        let pos = self.pos;
        self.swallowing.retain(|i| links.get(*i).is_some_and(|l| l.distance_xy(pos) <= Link::REACH));
    }

    fn relocate(&mut self, i: usize) {
        let link = &self.links[i];
        self.pos = self.area.place_arrival(link.to, link.to_hint);
        self.arming = LinkArming::placed_at(&self.links, self.pos);
        self.fired.push(i);
    }

    /// OK was pressed where the walker stands.
    pub(crate) fn press(&mut self) {
        self.presses += 1;
        if self.deaf_press > 0 {
            self.deaf_press -= 1;
            return;
        }
        if let Some(i) = self.links.iter().position(|l| l.needs_ok && l.is_touched_at(self.pos)) {
            self.relocate(i);
        }
    }
}

/// What a run of a bot found.
#[derive(Debug, Default)]
struct Run {
    /// When the exit was crossed (seconds), if it was.
    crossed_at: Option<f32>,
    notes: Vec<String>,
}

/// Drive `bot` (a travel bot) in `sim` for at most `secs` seconds at 100 steps a second, the way the headless loop
/// does: look, step, apply, and send a press when it wants one.
fn drive(bot: &mut TravelBot, sim: &mut Sim<'_>, exit: &Exit, secs: f32) -> Run {
    let dt = 0.01_f32;
    let mut run = Run::default();
    for tick in 0..(secs / dt) as usize {
        bot.observe(sim.area, sim.pos);
        let before = sim.pos;
        let step = bot.step(sim.area, sim.pos, dt, 360.0);
        sim.apply(step);
        if exit.is_crossed_by(before, sim.pos) {
            run.crossed_at = Some(tick as f32 * dt);
            break;
        }
        if bot.take_press().is_some() {
            sim.press();
        }
        run.notes.extend(bot.drain_notes());
    }
    run
}

fn to_next() -> TravelBot {
    TravelBot::new(vec!["next".to_string()], false)
}

#[test]
fn a_route_by_a_walk_on_link_and_then_a_ladder_that_needs_ok_reaches_the_exit_of_the_loft() {
    let area = Floors::new(true);
    let exit = area.exits().remove(0);
    let mut sim = Sim::new(&area, Pos::new(-200.0, 0.0, 0.0));
    let mut bot = to_next();
    let run = drive(&mut bot, &mut sim, &exit, 30.0);
    assert!(run.crossed_at.is_some(), "the bot crossed the exit of the loft: {:?} {:?}", sim.pos, run.notes);
    assert_eq!(sim.fired, [0, 1], "the walk-on link first, then the ladder: {:?}", run.notes);
    assert_eq!(sim.presses, 1, "one press, at the ladder");
    assert!(bot.gave_up().is_none());
    assert!(run.notes.iter().any(|n| n.contains("plan: 2 link(s) 0 1")), "the route it made is said: {:?}", run.notes);
    // (the Loft is over the East room's north wall: the exit is at the height of the Loft)
    assert!((sim.pos.z - 500.0).abs() < 1.0, "it crossed on the loft: {:?}", sim.pos);
}

#[test]
fn a_walker_goes_up_to_the_ladder_and_presses_there_and_nowhere_else() {
    let area = Floors::new(true);
    let exit = area.exits().remove(0);
    // already in the East room: only the ladder is needed
    let mut sim = Sim::new(&area, Pos::new(120.0, 0.0, 0.0));
    let mut bot = to_next();
    let dt = 0.01;
    let mut pressed_at = None;
    for _ in 0..3000 {
        bot.observe(sim.area, sim.pos);
        let step = bot.step(sim.area, sim.pos, dt, 360.0);
        sim.apply(step);
        assert!(!exit.is_crossed_by(sim.pos, sim.pos));
        if bot.wants_press() {
            assert!(sim.links[1].is_touched_at(sim.pos), "it wants the press at the foot of the ladder, not before: {:?}", sim.pos);
            pressed_at = Some(sim.pos);
            assert_eq!(bot.take_press(), Some(Press { npc: None }));
            assert!(bot.take_press().is_none(), "a press is taken once");
            sim.press();
            break;
        }
    }
    assert!(pressed_at.is_some(), "it came to the foot of the ladder");
    assert_eq!(sim.fired, [1]);
    assert!((sim.pos.x - 200.0).abs() < 1.0 && (sim.pos.y - 300.0).abs() < 1.0 && (sim.pos.z - 500.0).abs() < 1.0, "{:?}", sim.pos);
}

#[test]
fn a_walk_on_link_the_walker_is_put_down_next_to_is_left_and_come_back_to() {
    let area = Floors::new(true);
    let exit = area.exits().remove(0);
    // 15 units from the line of the first link: it is disarmed, and walking straight at it would never fire it
    let start = Pos::new(-125.0, 0.0, 0.0);
    let mut sim = Sim::new(&area, start);
    assert!(!sim.arming.is_armed(0));
    let mut bot = to_next();
    let run = drive(&mut bot, &mut sim, &exit, 40.0);
    assert!(run.crossed_at.is_some(), "{:?} {:?}", sim.pos, run.notes);
    assert_eq!(sim.fired, [0, 1], "{:?}", run.notes);
    // it walked out of the reach of the line (30 units) and back: the first note of its route is the link
    assert!(run.notes.iter().any(|n| n.contains("plan: 2 link(s) 0 1")), "{:?}", run.notes);
}

#[test]
fn a_walk_on_link_the_server_did_not_fire_is_walked_away_from_and_tried_again() {
    let area = Floors::new(true);
    let exit = area.exits().remove(0);
    let mut sim = Sim::new(&area, Pos::new(-200.0, 0.0, 0.0));
    sim.numb.insert(0); // the server does not agree that the link was armed: it fires after the walker has left its reach
    let mut bot = to_next();
    let run = drive(&mut bot, &mut sim, &exit, 40.0);
    assert!(run.crossed_at.is_some(), "{:?} {:?}", sim.pos, run.notes);
    assert_eq!(sim.fired, [0, 1], "it fired the second time: {:?}", run.notes);
    assert!(run.crossed_at.unwrap_or(0.0) > 2.0, "it waited for the link before it went round again: {:?}", run.crossed_at);
}

#[test]
fn a_press_that_gets_no_answer_is_made_again_and_a_ladder_that_never_answers_is_given_up() {
    let area = Floors::new(true);
    let exit = area.exits().remove(0);
    // answered at the second press
    let mut sim = Sim::new(&area, Pos::new(120.0, 0.0, 0.0));
    sim.deaf_press = 1;
    let mut bot = to_next();
    let run = drive(&mut bot, &mut sim, &exit, 30.0);
    assert!(run.crossed_at.is_some(), "{:?} {:?}", sim.pos, run.notes);
    assert_eq!((sim.presses, sim.fired.clone()), (2, vec![1]), "{:?}", run.notes);
    // never answered: three presses, then the route is made without the ladder, which leaves no way
    let mut sim = Sim::new(&area, Pos::new(120.0, 0.0, 0.0));
    sim.deaf_press = 99;
    let mut bot = to_next();
    let run = drive(&mut bot, &mut sim, &exit, 60.0);
    assert!(run.crossed_at.is_none());
    assert_eq!(sim.presses, MAX_TRIES, "{:?}", run.notes);
    assert!(run.notes.iter().any(|n| n.contains("did not fire after")), "{:?}", run.notes);
    assert!(bot.gave_up().is_some(), "with no ladder there is no way, and the bot says so in the end");
}

#[test]
fn a_walker_moved_by_the_server_to_somewhere_unexpected_gets_a_new_route_from_there() {
    let area = Floors::new(true);
    let exit = area.exits().remove(0);
    let mut sim = Sim::new(&area, Pos::new(-200.0, 0.0, 0.0));
    let mut bot = to_next();
    let dt = 0.01;
    // walk a little way, then the server puts the walker in the East room (a correction it did not expect)
    for _ in 0..20 {
        bot.observe(sim.area, sim.pos);
        let step = bot.step(sim.area, sim.pos, dt, 360.0);
        sim.apply(step);
    }
    assert!(sim.fired.is_empty());
    sim.pos = Pos::new(130.0, 20.0, 0.0);
    sim.arming = LinkArming::placed_at(&sim.links, sim.pos);
    let run = drive(&mut bot, &mut sim, &exit, 30.0);
    assert!(run.crossed_at.is_some(), "{:?} {:?}", sim.pos, run.notes);
    assert_eq!(sim.fired, [1], "only the ladder was needed from there: {:?}", run.notes);
    assert!(run.notes.iter().any(|n| n.contains("by the server")), "it noticed: {:?}", run.notes);
}

#[test]
fn where_the_surface_ends_at_the_exits_line_the_walker_goes_to_the_line_itself() {
    let area = Floors::new(false);
    let exit = area.exits().remove(0);
    let mut sim = Sim::new(&area, Pos::new(-200.0, 0.0, 0.0));
    let mut bot = to_next();
    let run = drive(&mut bot, &mut sim, &exit, 30.0);
    assert!(run.crossed_at.is_some(), "no surface past the line: the line itself is the goal: {:?} {:?}", sim.pos, run.notes);
    assert!((sim.pos.x - 300.0).abs() < 1.0, "{:?}", sim.pos);
}

#[test]
fn a_travel_bot_walks_round_the_walk_on_links_of_the_hall_to_its_exit() {
    let hall = RoomsAdapter::with_interactions().load_area(HALL).unwrap();
    let exit = hall.exits().remove(0);
    for start in [Pos::new(-100.0, 0.0, 0.0), Pos::new(100.0, 0.0, 0.0)] {
        let mut sim = Sim::new(hall.as_ref(), start);
        let mut bot = TravelBot::new(vec!["yard".to_string()], false);
        let run = drive(&mut bot, &mut sim, &exit, 20.0);
        assert!(run.crossed_at.is_some(), "from {start:?}: {:?} {:?}", sim.pos, run.notes);
        // the straight way from (-100, 0) to the exit passes over the line of the second link: it went round it
        assert!(sim.fired.is_empty(), "from {start:?} no link fired by accident: {:?}", sim.fired);
        assert!(bot.gave_up().is_none());
    }
}

#[test]
fn a_walker_knows_which_lines_it_may_walk_over_by_where_it_was_put_down() {
    let hall = RoomsAdapter::with_interactions().load_area(HALL).unwrap();
    let links = hall.links();
    // the first link leads to (150, 0): 20 units from the line of the second (x = 130), which is disarmed there
    let put = Pos::new(LINK_A_TO.0, LINK_A_TO.1, 0.0);
    let goal = Pos::new(100.0, 0.0, 0.0);
    let aim = |_: Pos| (goal, goal);
    let none = BTreeSet::new();
    let free = |_: Pos, _: Pos| false;
    let disarmed = LinkArming::placed_at(&links, put);
    assert!(!disarmed.is_armed(1));
    let route = plan_route(hall.as_ref(), &links, &disarmed, &none, put, &aim, false, &free).expect("a way");
    assert!(route.hops.is_empty());
    assert_eq!(route.last.len(), 2, "straight over the line it was put down next to, which cannot fire: {:?}", route.last);
    // The same walk from far away (the line armed) goes round it, or has no straight leg over the line.
    let armed = LinkArming::placed_at(&links, Pos::new(1.0e9, 1.0e9, 0.0));
    assert!(armed.is_armed(1));
    let route = plan_route(hall.as_ref(), &links, &armed, &none, put, &aim, false, &free).expect("a way round");
    assert!(route.last.len() > 2, "a walker that thinks the line is armed does not cross it: {:?}", route.last);
    assert!(route.last.windows(2).all(|leg| !links[1].is_touched_by(leg[0], leg[1])), "{:?}", route.last);
}

#[test]
fn the_navigator_keeps_the_arming_of_the_server_from_the_steps_it_sees_and_the_jumps() {
    let area = Floors::new(true);
    let mut nav = Navigator::new();
    assert!(nav.has_links(&area));
    assert_eq!(nav.links().len(), 2);
    // put down next to the first link's line: disarmed; walking away beyond its reach arms it
    nav.observe(Pos::new(-120.0, 0.0, 0.0));
    assert!(!nav.arming.is_armed(0));
    nav.observe(Pos::new(-130.0, 0.0, 0.0));
    assert!(!nav.arming.is_armed(0), "still within reach (20 units)");
    nav.observe(Pos::new(-150.0, 0.0, 0.0));
    assert!(nav.arming.is_armed(0), "40 units away: armed again");
    // a jump is a placement: next to the line again, it is disarmed again
    nav.observe(Pos::new(-115.0, 0.0, 0.0));
    assert!(nav.arming.is_armed(0), "(a short step is a step: the line was touched while armed)");
    nav.observe(Pos::new(130.0, 0.0, 0.0));
    assert!(nav.arming.is_armed(0), "far from everything: armed");
    nav.observe(Pos::new(-112.0, 0.0, 0.0));
    assert!(!nav.arming.is_armed(0), "a jump of hundreds of units puts the walker down: next to the line, disarmed");
}

#[test]
fn an_area_without_links_is_not_the_navigators_business() {
    let hall = RoomsAdapter::new().load_area(HALL).unwrap();
    let mut nav = Navigator::new();
    assert!(!nav.has_links(hall.as_ref()));
    let free = |_: Pos, _: Pos| false;
    let aim = |p: Pos| (p, p);
    assert_eq!(nav.step(hall.as_ref(), Pos::new(0.0, 0.0, 0.0), 1, &aim, false, &free, 0.01, 360.0), Nav::Here);
    assert!(!nav.wants_press());
    // a bot over such an area keeps its plain way (the plain tests of the travel bot cover it)
    let mut bot = TravelBot::new(vec!["yard".to_string()], false);
    assert!(!bot.wants_press());
    assert_eq!(bot.take_press(), None);
}
