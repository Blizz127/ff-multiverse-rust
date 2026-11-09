//! The bots on the owner's REAL reactor fields (read-only, `#[ignore]`; `FMV_FF7_DATA` is the game's `data/`
//! directory): the story bot and the travel bot walk `nmkin_3`, `nmkin_4` and `nmkin_5` by the links, talks and battle
//! points the adapter reads from the field scripts, against a mini server that applies the real server's rules (links
//! with their arming, OK presses, talks, a scripted battle that is won at once), and every step they announce is one
//! the server walks to from the last. Nothing of the game is kept or printed: ids, counts and the story's moment only.

use super::*;
use crate::bot::{TravelBot, BOT_SPEED_FACTOR};
use crate::nav::real;
use crate::state::NpcView;
use crate::story::StoryBot;
use fmv_adapter_api::{GameAdapter, StoryPoint, StoryTrigger};
use fmv_ff7::story::StoryState;
use std::sync::Arc;

/// The mini server's side of one field for one walker.
struct Srv<'a> {
    area: &'a dyn AreaRuntime,
    links: Vec<Link>,
    points: Vec<StoryPoint>,
    npcs: Vec<NpcView>,
    pos: Pos,
    arming: LinkArming,
    progress: Vec<u8>,
    /// What it did, in order: `link <i>`, `point <id>` (a step), `battle <formation>`.
    log: Vec<String>,
    presses: Vec<Option<u32>>,
    /// Steps it would have refused (the position the client announced is not one it walks to from the last).
    refused: u32,
}

impl<'a> Srv<'a> {
    fn new(area: &'a dyn AreaRuntime, npcs: Vec<NpcView>, pos: Pos, progress: Vec<u8>) -> Self {
        let links = area.links();
        let arming = LinkArming::placed_at(&links, pos);
        Self { area, links, points: area.story_points(), npcs, pos, arming, progress, log: Vec::new(), presses: Vec::new(), refused: 0 }
    }

    fn relocate(&mut self, i: usize) {
        let l = &self.links[i];
        self.pos = self.area.place_arrival(l.to, l.to_hint);
        self.arming = LinkArming::placed_at(&self.links, self.pos);
        self.log.push(format!("link {i}"));
    }

    fn apply_point(&mut self, id: u32) {
        if let Some(step) = self.area.story_advance(&self.progress, id) {
            self.log.push(format!("point {id}"));
            self.progress = step.progress;
        }
    }

    fn moment(&self) -> u16 {
        StoryState::decode(&self.progress).map_or(0, |s| s.moment)
    }

    fn walked(&mut self, to: Pos) {
        let from = self.pos;
        let MoveResult::Accepted(to) = self.area.validate_move(from, to) else {
            self.refused += 1;
            return;
        };
        self.pos = to;
        if let Some(i) = self.arming.step(&self.links, from, to) {
            self.relocate(i);
            return;
        }
        let hit: Vec<u32> = self.points.iter().filter(|p| p.is_hit_by(from, to)).map(|p| p.id).collect();
        for id in hit {
            let battle = self.area.story_battle(id);
            // a battle point fires by coming to it from outside only
            if battle.is_some() && self.points.iter().find(|p| p.id == id).is_some_and(|p| p.is_hit_by(from, from)) {
                continue;
            }
            if let (Some(formation), true) = (battle, self.area.story_advance(&self.progress, id).is_some()) {
                self.log.push(format!("battle {formation}"));
            }
            self.apply_point(id);
        }
    }

    fn press(&mut self, npc: Option<u32>) {
        self.presses.push(npc);
        let addressed = npc.and_then(|id| self.npcs.iter().find(|n| n.id == id)).map(|n| n.pos);
        let talking = addressed.is_some();
        let point = self
            .points
            .iter()
            .find(|p| matches!(p.trigger, StoryTrigger::Talk { .. }) == talking && p.is_pressed_at(self.pos, addressed) && self.area.story_advance(&self.progress, p.id).is_some())
            .map(|p| p.id);
        if let Some(id) = point {
            self.apply_point(id);
        } else if let (false, Some(i)) = (talking, self.links.iter().position(|l| l.needs_ok && l.is_touched_at(self.pos))) {
            self.relocate(i);
        }
    }
}

/// What a field's run found.
struct Found {
    crossed: bool,
    log: Vec<String>,
    presses: Vec<Option<u32>>,
    moment: u16,
    refused: u32,
    gave_up: Option<String>,
    /// What the bots said about their routes (the links taken, the routes made), for a failure's message.
    notes: Vec<String>,
}

/// The bots in `field` (entered from `from`) with the story at `moment`: the story bot first, then the travel bot to
/// `exit_to`, the way the headless loop decides; for at most a few minutes of walking.
fn run_field(field: &str, from: &str, moment: u16, exit_to: &str) -> Found {
    run_field_with(field, from, StoryState::at(moment).encode(), exit_to)
}

/// [`run_field`] for a story that is more than a moment (the lift down, the countdown's moment 27).
fn run_field_with(field: &str, from: &str, progress: Vec<u8>, exit_to: &str) -> Found {
    let adapter = real::adapter().expect("FMV_FF7_DATA");
    let (area, start) = real::arrival(&adapter, field, from);
    let npcs: Vec<NpcView> = area
        .npcs()
        .iter()
        .enumerate()
        .map(|(i, n)| NpcView { id: i as u32, name: n.entity_name.clone(), model: n.appearance.model.clone(), idle_animation: None, pos: n.pos, facing: n.facing })
        .collect();
    let content: Arc<dyn GameAdapter> = Arc::new(real::adapter().expect("FMV_FF7_DATA"));
    assert!(area.exits().iter().any(|e| e.target == exit_to), "{field} has an exit to {exit_to}");
    let mut srv = Srv::new(&area, npcs.clone(), start, progress);
    let mut story = StoryBot::new(true, false).with_adapter(Some(content));
    story.see_npcs(&npcs);
    story.see_party(1, 0);
    story.set_wanted_exit(Some(exit_to));
    let mut travel = TravelBot::new(vec![exit_to.to_string()], false);
    let dt = 0.01_f32;
    let speed = area.max_speed() * BOT_SPEED_FACTOR;
    let mut crossed = false;
    let mut notes: Vec<String> = Vec::new();
    for _ in 0..30_000 {
        story.observe(&area, srv.pos);
        travel.observe(&area, srv.pos);
        let by_story = story.wants_to_walk(&area, &srv.progress, srv.pos);
        let before = srv.pos;
        let step = if by_story { story.step(&area, srv.pos, dt, speed, &srv.progress) } else { travel.step(&area, srv.pos, dt, speed) };
        srv.walked(step.pos);
        // (a line is crossed for good only when an exit on it is open for the story the walker holds: the lift's way up is
        // closed while the lift is down, and the server walks a player over a line whose exits are all closed)
        let open = area.exits().iter().any(|e| e.target == exit_to && e.is_crossed_by(before, srv.pos) && area.exit_state(e, &srv.progress) == fmv_adapter_api::ExitState::Open);
        if !by_story && open {
            crossed = true;
            break;
        }
        if let Some(press) = if by_story { story.take_press() } else { travel.take_press() } {
            srv.press(press.npc);
        }
        notes.extend(story.drain_notes());
        notes.extend(travel.drain_notes());
        if story.gave_up().is_some() || travel.gave_up().is_some() {
            break;
        }
    }
    let gave_up = story.gave_up().or(travel.gave_up()).map(str::to_string);
    Found { crossed, log: srv.log.clone(), presses: srv.presses.clone(), moment: srv.moment(), refused: srv.refused, gave_up, notes }
}

/// `nmkin_3` to `nmkin_5`: the walkmesh of these fields is cut into pieces that only the fields' ladders and jumps join
/// (`nmkin_3` has six links, `nmkin_4` two), so on foot there is no way on past `nmkin_3`. A story bot that talks (the
/// first field's woman and the last field's materia), a travel bot that takes the links it needs, and no step the
/// server would refuse.
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_DATA)"]
fn the_bots_walk_the_reactor_fields_past_the_stair_by_the_links_the_adapter_reads_and_do_the_talks_on_the_way() {
    // nmkin_3: a jump, the talk that moves the story from 13 to 14, then two ladders that wait for OK, then the gateway
    let found = run_field("nmkin_3", "nmkin_2", 13, "nmkin_4");
    assert!(found.crossed, "nmkin_3: the bot crossed the line to nmkin_4 ({:?})", found.gave_up);
    assert_eq!(found.refused, 0, "every announced step is one the server walks to");
    assert_eq!(found.moment, 14, "the woman's talk moved the story");
    assert_eq!(found.presses.len(), 3, "a talk and two ladders: {:?}", found.presses);
    assert!(found.presses[0].is_some() && found.presses[1..].iter().all(Option::is_none), "the talk addressed a character, the ladders nobody: {:?}", found.presses);
    // (the jump, the woman's step, the two ladders)
    let kinds: Vec<&str> = found.log.iter().map(|l| l.split(' ').next().unwrap_or("")).collect();
    assert_eq!(kinds, ["link", "point", "link", "link"], "{:?}", found.log);
    // nmkin_4: a ladder that needs no key, then the gateway on the lower level; nothing to press
    let found = run_field("nmkin_4", "nmkin_3", 14, "nmkin_5");
    assert!(found.crossed, "nmkin_4: the bot crossed the line to nmkin_5 ({:?})", found.gave_up);
    assert_eq!(found.refused, 0);
    assert!(found.presses.is_empty(), "the ladder down needs no key: {:?}", found.presses);
    assert_eq!(found.log.iter().filter(|l| l.starts_with("link")).count(), 1, "{:?}", found.log);
    // nmkin_5: the materia's talk (14 -> 15) first, then the battle's triangle (15 -> 27), which the bot walked round on
    // the way to the materia
    let found = run_field("nmkin_5", "nmkin_4", 14, "nmkin_4");
    assert!(found.crossed, "nmkin_5: the bot crossed a line out ({:?})", found.gave_up);
    assert_eq!(found.refused, 0);
    assert_eq!(found.moment, 27, "the talk and the won battle");
    assert_eq!(found.presses.len(), 1, "{:?}", found.presses);
    assert!(found.presses[0].is_some(), "the materia was talked to");
    let talk = found.log.iter().position(|l| l.starts_with("point")).expect("the talk's step");
    let battle = found.log.iter().position(|l| l.starts_with("battle")).expect("the battle");
    assert!(talk < battle, "the talk comes before the battle: {:?}", found.log);
}

/// The story the leader holds when the Guard Scorpion has been won: moment 27 and the lift down (what the descent left).
fn at_the_bomb() -> Vec<u8> {
    let mut s = StoryState::at(27);
    assert!(s.set_bit(1, 225, 0, true));
    s.encode()
}

/// The way back up, at moment 27 (the Guard Scorpion won, the countdown running): each reactor field from where the party
/// arrives coming UP to the exit toward `nrthmk`, with the story the leader holds (the lift down), by the same links the
/// way down used, in the other direction.
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_DATA)"]
fn the_bots_climb_back_up_through_the_reactor_fields_at_the_moment_the_bomb_is_set() {
    for (field, from, to) in [
        ("nmkin_5", "nmkin_4", "nmkin_4"),
        ("nmkin_4", "nmkin_5", "nmkin_3"),
        ("nmkin_3", "nmkin_4", "nmkin_2"),
        ("nmkin_2", "nmkin_3", "elevtr1"),
        ("nmkin_1", "elevtr1", "nrthmk"),
    ] {
        let found = run_field_with(field, from, at_the_bomb(), to);
        println!(
            "{field} -> {to}: crossed {} log {:?} presses {} moment {} refused {} gave_up {:?}",
            found.crossed,
            found.log,
            found.presses.len(),
            found.moment,
            found.refused,
            found.gave_up
        );
        assert!(found.crossed, "{field}: the bot crossed the line to {to} ({:?}; log {:?}; notes {:#?})", found.gave_up, found.log, found.notes);
        assert_eq!(found.refused, 0, "{field}: every announced step is one the server walks to");
        assert_eq!(found.moment, 27, "{field}: nothing on the way moves the story");
    }
    // nmkin_3: the story bot talks to the woman first (the one step of the way up that is its own: she is freed, which moves
    // no moment) and then the travel bot takes the links it needs to the gateway
    let found = run_field_with("nmkin_3", "nmkin_4", at_the_bomb(), "nmkin_2");
    assert_eq!(found.log.iter().filter(|l| l.starts_with("point")).collect::<Vec<_>>(), ["point 0"], "{:?}", found.log);
    assert_eq!(found.presses.iter().filter(|p| p.is_some()).count(), 1, "one talk: {:?}", found.presses);
    assert!(found.log.first().is_some_and(|l| l.starts_with("link") || l.starts_with("point")), "{:?}", found.log);
}

/// The lift, both ways, with the bots of the headless loop (the story bot first, told where the route goes next). Going
/// UP at moment 27 with the lift down: ONE press (the panel's ride back, a step that is not forward but opens the way to
/// `nmkin_1`), then the line is crossed, and the ride down (forward again once the lift is up, but it would close the way)
/// is not pursued: no flapping. Going DOWN the first time at moment 11 with the lift up: one press (the ride down, which
/// moves the story to 12), then the line to `nmkin_2`.
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_DATA)"]
fn the_bots_ride_the_lift_once_each_way_and_never_flap() {
    let up = run_field_with("elevtr1", "nmkin_2", at_the_bomb(), "nmkin_1");
    assert!(up.crossed, "up: the bot got out through the line to nmkin_1 ({:?}; log {:?}; notes {:#?})", up.gave_up, up.log, up.notes);
    assert_eq!(up.presses.len(), 1, "one press, the panel's: {:?}", up.presses);
    assert_eq!(up.log, ["point 1"], "the ride back (the panel's second point) and nothing else");
    assert_eq!((up.moment, up.refused), (27, 0));

    let down = run_field_with("elevtr1", "nmkin_1", StoryState::at(11).encode(), "nmkin_2");
    assert!(down.crossed, "down: the bot got out through the line to nmkin_2 ({:?}; log {:?}; notes {:#?})", down.gave_up, down.log, down.notes);
    assert_eq!(down.presses.len(), 1, "one press, the panel's: {:?}", down.presses);
    assert_eq!(down.log, ["point 0"], "the ride down");
    assert_eq!((down.moment, down.refused), (12, 0));
}
