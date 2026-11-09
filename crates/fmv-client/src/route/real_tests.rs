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
}

/// The bots in `field` (entered from `from`) with the story at `moment`: the story bot first, then the travel bot to
/// `exit_to`, the way the headless loop decides; for at most a few minutes of walking.
fn run_field(field: &str, from: &str, moment: u16, exit_to: &str) -> Found {
    let adapter = real::adapter().expect("FMV_FF7_DATA");
    let (area, start) = real::arrival(&adapter, field, from);
    let npcs: Vec<NpcView> = area
        .npcs()
        .iter()
        .enumerate()
        .map(|(i, n)| NpcView { id: i as u32, name: n.entity_name.clone(), model: n.appearance.model.clone(), idle_animation: None, pos: n.pos, facing: n.facing })
        .collect();
    let content: Arc<dyn GameAdapter> = Arc::new(real::adapter().expect("FMV_FF7_DATA"));
    let exit = area.exits().into_iter().find(|e| e.target == exit_to).expect("the exit");
    let mut srv = Srv::new(&area, npcs.clone(), start, StoryState::at(moment).encode());
    let mut story = StoryBot::new(true, false).with_adapter(Some(content));
    story.see_npcs(&npcs);
    story.see_party(1, 0);
    let mut travel = TravelBot::new(vec![exit_to.to_string()], false);
    let dt = 0.01_f32;
    let speed = area.max_speed() * BOT_SPEED_FACTOR;
    let mut crossed = false;
    for _ in 0..30_000 {
        story.observe(&area, srv.pos);
        travel.observe(&area, srv.pos);
        let by_story = story.wants_to_walk(&area, &srv.progress, srv.pos);
        let before = srv.pos;
        let step = if by_story { story.step(&area, srv.pos, dt, speed, &srv.progress) } else { travel.step(&area, srv.pos, dt, speed) };
        srv.walked(step.pos);
        if !by_story && exit.is_crossed_by(before, srv.pos) {
            crossed = true;
            break;
        }
        if let Some(press) = if by_story { story.take_press() } else { travel.take_press() } {
            srv.press(press.npc);
        }
        if story.gave_up().is_some() || travel.gave_up().is_some() {
            break;
        }
    }
    let gave_up = story.gave_up().or(travel.gave_up()).map(str::to_string);
    Found { crossed, log: srv.log.clone(), presses: srv.presses.clone(), moment: srv.moment(), refused: srv.refused, gave_up }
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
