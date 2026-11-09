//! The story bot's press, talk and battle points, and its way through an area with links: on the FIXTURE hall and yard
//! with their interactions (`RoomsAdapter::with_interactions`) and on the mock area of `crate::route::tests`.

use super::*;
use crate::route::tests::{Floors, Sim};
use fmv_adapter_api::{AdapterError, AreaInfo, Bounds, GameAdapter, StoryStep};
use fmv_core::GameId;
use fmv_ff7::story::StoryState;
use fmv_fixture::rooms::{RoomsAdapter, BATTLE_REGION, HALL, STORY_ADVANCED, TALK_RADIUS, YARD, YARD_NPC_POS};

fn guard(id: u32, x: f32, y: f32) -> NpcView {
    NpcView { id, name: "guard".into(), model: "guardm".into(), idle_animation: None, pos: Pos::new(x, y, 0.0), facing: 0.0 }
}

/// Step `bot` from `pos` until `done` says so (at most `max` steps of 10 ms at 360 units a second).
fn walk_until(bot: &mut StoryBot, area: &dyn AreaRuntime, mut pos: Pos, progress: &[u8], max: usize, mut done: impl FnMut(&StoryBot, Pos) -> bool) -> Pos {
    for _ in 0..max {
        if done(bot, pos) {
            break;
        }
        pos = bot.step(area, pos, 0.01, 360.0, progress).pos;
    }
    pos
}

#[test]
fn the_bot_walks_into_a_press_region_stands_there_and_wants_ok_pressed_once_and_never_again_if_nothing_happens() {
    let area = RoomsAdapter::with_interactions().load_area(HALL).unwrap();
    let press = area.story_points().into_iter().find(|p| p.id == 1).expect("the press region");
    assert!(matches!(press.trigger, StoryTrigger::Press(_)));
    let mut bot = StoryBot::new(true, false);
    // near the press region, it is the nearest point (the walk-over region and the battles are further or come last)
    let start = Pos::new(100.0, 130.0, 0.0);
    assert!(bot.target(area.as_ref(), &[], start).is_some_and(|t| t.point.id == 1 && t.kind == Kind::Press && !t.battle));
    assert!(bot.wants_to_walk(area.as_ref(), &[], start));
    let pos = walk_until(&mut bot, area.as_ref(), start, &[], 2000, |b, _| b.wants_press());
    assert!(bot.wants_press(), "it came to the region");
    assert!(press.is_pressed_at(pos, None), "and stands in it: {pos:?}");
    // It stands still while the press is wanted, and the press is taken once.
    let held = bot.step(area.as_ref(), pos, 0.01, 360.0, &[]);
    assert!(!held.moved && held.pos == pos);
    assert_eq!(bot.take_press(), Some(Press { npc: None }));
    assert!(!bot.wants_press() && bot.take_press().is_none());
    // The step does not come: the press is NOT made again (a second press on a story the server has moved meanwhile
    // would be another step: the lift's panel rides back up), and after the patience the bot reports it and stops.
    let mut more = 0;
    for _ in 0..((PRESS_PATIENCE_S * 100.0) as usize + 100) {
        let step = bot.step(area.as_ref(), pos, 0.01, 360.0, &[]);
        assert!(!step.moved);
        if bot.take_press().is_some() {
            more += 1;
        }
    }
    assert_eq!(more, 0, "a press is made once");
    assert!(bot.gave_up().is_some_and(|why| why.contains("pressed OK") && why.contains("did not move")), "{:?}", bot.gave_up());
}

#[test]
fn a_story_that_has_not_moved_yet_does_not_make_the_bot_press_the_same_point_twice() {
    let area = RoomsAdapter::with_interactions().load_area(HALL).unwrap();
    let mut bot = StoryBot::new(true, false);
    walk_until(&mut bot, area.as_ref(), Pos::new(100.0, 130.0, 0.0), &[], 2000, |b, _| b.wants_press());
    assert!(bot.take_press().is_some());
    // The bot is pushed out of the region (a correction) and walks back in: it is still waiting for the first press's
    // step, and presses nothing.
    let mut pos = Pos::new(100.0, 130.0, 0.0);
    for _ in 0..2000 {
        let step = bot.step(area.as_ref(), pos, 0.01, 360.0, &[]);
        pos = step.pos;
        assert!(!bot.wants_press() && bot.take_press().is_none(), "no second press while the step is awaited: {pos:?}");
    }
    // The step comes: the progress is another, and what is pressed next is judged on the new story.
    let after = bot.step(area.as_ref(), pos, 0.01, 360.0, STORY_ADVANCED);
    assert!(!after.moved);
    assert!(!bot.wants_press());
}

/// A game that is not the fixture's and whose progressions join like FF7's (the further moment, the union of bits).
struct Ff7Join;

impl GameAdapter for Ff7Join {
    fn game(&self) -> GameId {
        GameId::Ff7
    }
    fn list_areas(&self) -> Vec<AreaInfo> {
        Vec::new()
    }
    fn load_area(&self, key: &str) -> Result<Box<dyn AreaRuntime>, AdapterError> {
        Err(AdapterError::AreaNotFound(key.to_string()))
    }
    fn merge_progression(&self, stored: &[u8], advanced: &[u8]) -> Vec<u8> {
        fmv_ff7::story::merge_progression(stored, advanced)
    }
}

#[test]
fn a_step_is_forward_when_the_games_own_join_of_it_and_the_story_is_something_new() {
    let bot = StoryBot::new(true, false).with_adapter(Some(Arc::new(Ff7Join)));
    let at = |moment: u16, lift: bool| {
        let mut state = StoryState::at(moment);
        if lift {
            assert!(state.set_bit(1, 225, 0, true));
        }
        state.encode()
    };
    // the ride down (a further moment, the lift bit on), a talk (a further moment), a new account's first step
    assert!(bot.is_forward(&at(11, false), &at(12, true)));
    assert!(bot.is_forward(&at(13, false), &at(14, false)));
    assert!(bot.is_forward(&[], &at(12, false)));
    // the ride back: the same moment, the lift bit off: the join adds nothing, and that is no step forward
    assert!(!bot.is_forward(&at(12, true), &at(12, false)));
    // nor is a step to where the story already is, or behind it
    assert!(!bot.is_forward(&at(12, true), &at(12, true)));
    assert!(!bot.is_forward(&at(14, false), &at(13, false)));
    // a bot with no content to judge by pursues every step the area offers
    assert!(StoryBot::new(true, false).is_forward(&at(12, true), &at(12, false)));
}

#[test]
fn the_toggle_region_that_takes_the_story_back_is_never_pursued_after_the_press_region_has_paid() {
    let adapter: Arc<dyn GameAdapter> = Arc::new(RoomsAdapter::with_interactions());
    let area = adapter.load_area(HALL).unwrap();
    // The toggle region (point 4): OK pressed there takes an advanced account back to a new account's story. The area
    // offers that step (the server applies a press as the point says), but it is not forward.
    assert!(area.story_advance(STORY_ADVANCED, 4).is_some());
    let in_the_toggle = Pos::new(200.0, 200.0, 0.0);
    let naive = StoryBot::new(true, false);
    assert!(naive.target(area.as_ref(), STORY_ADVANCED, in_the_toggle).is_some_and(|t| t.point.id == 4), "a bot that takes every step goes for it");
    let mut bot = StoryBot::new(true, false).with_adapter(Some(Arc::clone(&adapter)));
    assert!(bot.target(area.as_ref(), STORY_ADVANCED, in_the_toggle).is_none());
    assert!(!bot.wants_to_walk(area.as_ref(), STORY_ADVANCED, in_the_toggle));
    let step = bot.step(area.as_ref(), in_the_toggle, 0.01, 360.0, STORY_ADVANCED);
    assert!(!step.moved && !bot.wants_press(), "it stands where it is and presses nothing");
    // For a new account the toggle does not apply at all, and the press region is the nearest step forward.
    assert!(bot.target(area.as_ref(), &[], in_the_toggle).is_some_and(|t| t.point.id == 1 && t.kind == Kind::Press));
    // The whole way: press the press region on the new story, and once the step has come, press nothing more.
    let pos = walk_until(&mut bot, area.as_ref(), in_the_toggle, &[], 3000, |b, _| b.wants_press());
    assert!(bot.take_press().is_some(), "the press region is pressed once");
    for _ in 0..3000 {
        bot.step(area.as_ref(), pos, 0.01, 360.0, STORY_ADVANCED);
        assert!(!bot.wants_press() && bot.take_press().is_none(), "the story moved on: the toggle is not pressed");
    }
}

#[test]
fn a_press_that_moved_the_story_leaves_the_bot_with_nothing_to_press() {
    // (with the game's own content, as a bot in a session has: the toggle region's step, which takes an advanced story
    // back, is none to pursue)
    let adapter: Arc<dyn GameAdapter> = Arc::new(RoomsAdapter::with_interactions());
    let area = adapter.load_area(HALL).unwrap();
    let mut bot = StoryBot::new(true, false).with_adapter(Some(adapter));
    let pos = walk_until(&mut bot, area.as_ref(), Pos::new(100.0, 130.0, 0.0), &[], 2000, |b, _| b.wants_press());
    assert!(bot.take_press().is_some());
    // the server's step comes back: the account's story has moved on, and no point applies
    let after = bot.step(area.as_ref(), pos, 0.01, 360.0, STORY_ADVANCED);
    assert!(!after.moved && !bot.wants_press() && bot.take_press().is_none());
    assert!(!bot.wants_to_walk(area.as_ref(), STORY_ADVANCED, pos));
    assert!(bot.gave_up().is_none());
}

#[test]
fn a_talk_point_is_walked_up_to_and_pressed_addressing_the_character_the_snapshot_named() {
    let yard = RoomsAdapter::with_interactions().load_area(YARD).unwrap();
    let start = Pos::new(-200.0, 0.0, 0.0);
    let mut bot = StoryBot::new(true, false);
    // the area has not announced the guard: there is nobody to talk to
    assert!(!bot.wants_to_walk(yard.as_ref(), &[], start));
    // somebody who does not stand at the point's place is not the character of the point
    bot.see_npcs(&[guard(0, 0.0, 0.0)]);
    assert!(!bot.wants_to_walk(yard.as_ref(), &[], start));
    // the guard stands there: the bot goes, and stops when it is near enough to talk
    bot.see_npcs(&[guard(0, 0.0, 0.0), guard(5, YARD_NPC_POS.0, YARD_NPC_POS.1)]);
    assert!(bot.wants_to_walk(yard.as_ref(), &[], start));
    let pos = walk_until(&mut bot, yard.as_ref(), start, &[], 3000, |b, _| b.wants_press());
    let guard_at = Pos::new(YARD_NPC_POS.0, YARD_NPC_POS.1, 0.0);
    assert!(bot.wants_press(), "it came up to the guard (at {pos:?})");
    assert!(pos.dist_xy(guard_at) <= TALK_RADIUS * 0.7 + 1.0, "within the talk radius, with room to spare: {pos:?}");
    assert!(pos.dist_xy(guard_at) > 1.0, "not on top of it");
    assert_eq!(bot.take_press(), Some(Press { npc: Some(5) }), "it addresses the character that stands at the point (id 5 of the list)");
    // the step came: nothing is left to say
    assert!(!bot.wants_to_walk(yard.as_ref(), STORY_ADVANCED, pos));
}

/// The hall with only the first battle region left as a point (point 2: the party fights formation 0 first, a won
/// battle is the step), so that the order of the points does not hide it.
struct OnlyBattle(Box<dyn AreaRuntime>);

impl AreaRuntime for OnlyBattle {
    fn info(&self) -> &AreaInfo {
        self.0.info()
    }
    fn spawn_points(&self) -> Vec<Pos> {
        self.0.spawn_points()
    }
    fn validate_move(&self, from: Pos, to: Pos) -> MoveResult {
        self.0.validate_move(from, to)
    }
    fn bounds(&self) -> Bounds {
        self.0.bounds()
    }
    fn walk_triangles(&self) -> Vec<[Pos; 3]> {
        self.0.walk_triangles()
    }
    fn max_speed(&self) -> f32 {
        self.0.max_speed()
    }
    fn exits(&self) -> Vec<Exit> {
        self.0.exits()
    }
    fn story_points(&self) -> Vec<StoryPoint> {
        self.0.story_points().into_iter().filter(|p| p.id == 2).collect()
    }
    fn story_advance(&self, progress: &[u8], point: u32) -> Option<StoryStep> {
        self.0.story_advance(progress, point)
    }
    fn story_battle(&self, point: u32) -> Option<u16> {
        self.0.story_battle(point)
    }
}

/// Did any step of `bot` over `max` steps touch `point`? And where did it stand at the end?
fn touches(bot: &mut StoryBot, area: &dyn AreaRuntime, start: Pos, point: &StoryPoint, max: usize) -> (Option<usize>, Pos) {
    let mut pos = start;
    for i in 0..max {
        let step = bot.step(area, pos, 0.01, 360.0, &[]);
        if point.is_hit_by(pos, step.pos) {
            return (Some(i), step.pos);
        }
        pos = step.pos;
    }
    (None, pos)
}

#[test]
fn a_bot_that_leads_a_party_waits_before_a_battle_point_until_the_others_are_in_view_but_not_for_ever() {
    let area = OnlyBattle(RoomsAdapter::with_interactions().load_area(HALL).unwrap());
    let point = area.story_points().remove(0);
    assert_eq!(area.story_battle(point.id), Some(fmv_fixture::rooms::BATTLE_FORMATION));
    let start = Pos::new(-100.0, 0.0, 0.0);
    // a party of two and nobody in view: it walks up to the region and stops short of it
    let mut bot = StoryBot::new(true, false);
    bot.see_party(2, 0);
    let (touched, pos) = touches(&mut bot, &area, start, &point, 1500);
    assert_eq!(touched, None, "the region was not touched: {pos:?}");
    assert!(pos.dist_xy(Pos::new(BATTLE_REGION[0].0, BATTLE_REGION[0].1, 0.0)) < 150.0, "it waits at the region: {pos:?}");
    assert!(bot.gave_up().is_none() && bot.wants_to_walk(&area, &[], pos));
    // the other is in view now: it goes in
    bot.see_party(2, 1);
    let (touched, _) = touches(&mut bot, &area, pos, &point, 600);
    assert!(touched.is_some(), "with the party in view the bot walks onto the battle");
    // alone (no party, or a party of one) it does not wait at all
    for party in [(0, 0), (1, 0)] {
        let mut lone = StoryBot::new(true, false);
        lone.see_party(party.0, party.1);
        assert!(touches(&mut lone, &area, start, &point, 1500).0.is_some(), "{party:?}");
    }
    // a party that never shows up is not waited for for ever
    let mut patient = StoryBot::new(true, false);
    patient.see_party(2, 0);
    let (touched, _) = touches(&mut patient, &area, start, &point, (BATTLE_PARTY_WAIT_S * 100.0) as usize + 1500);
    assert!(touched.is_some(), "after {BATTLE_PARTY_WAIT_S} s it goes in");
}

#[test]
fn the_points_that_start_no_battle_come_first_and_the_battles_last() {
    let area = RoomsAdapter::with_interactions().load_area(HALL).unwrap();
    let bot = StoryBot::new(true, false);
    // standing in the middle of the first battle region, with the walk-over region and the press region further away
    let here = Pos::new(-275.0, 225.0, 0.0);
    let target = bot.target(area.as_ref(), &[], here).expect("something applies to a new account");
    assert!(!target.battle, "a point of the story that starts no battle comes before one that does: {:?}", target.point.label);
    // with only the battles left, the nearest is the first
    let battles = OnlyBattle(RoomsAdapter::with_interactions().load_area(HALL).unwrap());
    assert!(bot.target(&battles, &[], here).is_some_and(|t| t.battle && t.point.id == 2));
}

#[test]
fn a_bot_in_an_area_with_links_goes_by_the_links_to_the_press_point_and_presses_there() {
    let area = Floors::new(true).with_story();
    let mut sim = Sim::new(&area, Pos::new(-200.0, 0.0, 0.0));
    let mut bot = StoryBot::new(true, false);
    let mut taken = Vec::new();
    for _ in 0..4000 {
        bot.observe(&area, sim.pos);
        let step = bot.step(&area, sim.pos, 0.01, 360.0, &[]);
        sim.apply(step);
        if let Some(press) = bot.take_press() {
            taken.push((press, sim.pos));
            sim.press();
            if sim.presses >= 2 {
                break;
            }
        }
    }
    assert_eq!(sim.fired, [0, 1], "the walk-on link, then the ladder: {:?}", bot.drain_notes());
    // the first press was the ladder's (nobody addressed); the second the panel's, in the loft
    assert_eq!(taken.len(), 2, "{taken:?}");
    assert_eq!(taken[0].0, Press { npc: None });
    assert_eq!(taken[1].0, Press { npc: None });
    let (_, at) = taken[1];
    assert!((at.z - 500.0).abs() < 1.0 && (180.0..=220.0).contains(&at.x) && (280.0..=320.0).contains(&at.y), "the panel's press is in the loft's region: {at:?}");
    assert!(bot.gave_up().is_none());
}

#[test]
fn a_bot_going_for_another_point_keeps_off_the_region_of_a_battle_on_the_way() {
    let area = RoomsAdapter::with_interactions().load_area(HALL).unwrap();
    let points = area.story_points();
    let battles: Vec<&StoryPoint> = points.iter().filter(|p| area.story_battle(p.id).is_some()).collect();
    assert!(battles.len() >= 2);
    // From the west of the first battle region, the straight way to the press region (point 1) goes over it.
    let start = Pos::new(-350.0, 225.0, 0.0);
    let press = points.iter().find(|p| p.id == 1).unwrap();
    assert!(battles.iter().any(|b| b.is_hit_by(start, press.center())), "the straight way touches a battle region");
    let mut bot = StoryBot::new(true, false);
    assert!(bot.target(area.as_ref(), &[], start).is_some_and(|t| t.point.id == 1), "the press region is the nearest point that starts no battle");
    let mut pos = start;
    for _ in 0..4000 {
        if bot.wants_press() {
            break;
        }
        let step = bot.step(area.as_ref(), pos, 0.01, 360.0, &[]);
        assert!(battles.iter().all(|b| !b.is_hit_by(pos, step.pos)), "a step from {pos:?} to {:?} touches a battle region", step.pos);
        pos = step.pos;
    }
    assert!(bot.wants_press(), "it came to the press region round the battle (at {pos:?})");
    assert!(press.is_pressed_at(pos, None));
    // Going for the battle itself, on the other hand, is going over its region.
    let only = OnlyBattle(RoomsAdapter::with_interactions().load_area(HALL).unwrap());
    let point = only.story_points().remove(0);
    let mut lone = StoryBot::new(true, false);
    assert!(touches(&mut lone, &only, start, &point, 2000).0.is_some());
}
