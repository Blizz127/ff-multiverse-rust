//! The OK button and the lines that move a player inside an area: `ClientMsg::Interact` and the area's links.
//!
//! **Presses.** A client sends `Interact` once per press of OK (Enter or Space), naming the character it addressed if
//! it found one in reach. [`World::press`] works out what OK does from where the server holds the player, never from
//! what the client says it stands on: with a character addressed, the talk points of the area that character belongs
//! to (`StoryTrigger::Talk`: the player is in reach of it, and the character is the point's: FF7's talk scripts that
//! move the story); with none, the press points under the player's feet (`StoryTrigger::Press`: the elevator's panel)
//! and then the links that wait for OK (`Link::needs_ok`: the ladders), whichever the player touches. The step of a
//! story point is judged on the story as it was before the press, and the first point that applies is the press's
//! (the lift's two points, the ride down and the ride up, exclude each other and are one press each). A press that
//! reaches nothing is answered with nothing. A player in a battle, in transit, or fenced (below) presses nothing.
//!
//! **Links.** A link puts the player somewhere else in the same area: the walk-on kind fires when the player walks
//! up to its line while it is armed (`fmv_adapter_api::LinkArming`: a player placed anywhere starts with the lines
//! within reach of it disarmed, and a disarmed line arms again once the player has been out of reach of it), the OK
//! kind on a press while the player touches the line. The player is placed at the link's destination on the
//! destination's own surface, and the client is told with `MoveRejected { seq, corrected }`, the message that already
//! puts a client where the server says it is: no wire change.
//!
//! **The fence.** `MoveRejected` of an intent is only taken by a client when no newer intent has been sent
//! (`fmv_client::state`, "How the own player is reconciled"), and the intents already on their way are judged from
//! the new place: a short link that changes level (a ladder) could make one of them walk the player on, on the new
//! level, while the client goes on walking on the old one. So after a relocation every intent is answered with the
//! destination, spending no budget, until one agrees with it (within [`FENCE_XY`] of it and at its height: the
//! client's own resync after it took the correction is exactly that) or [`FENCE_TIMEOUT`] has passed.

use std::time::{Duration, Instant};

use fmv_adapter_api::{Link, LinkArming, StoryTrigger};
use fmv_core::{EntityId, Pos};

use super::World;

/// How near (x and y) to the destination of a link an intent has to be to show that its client has taken the move.
pub(super) const FENCE_XY: f32 = 60.0;
/// How far from the destination's height an intent may be (the level the client is on).
pub(super) const FENCE_Z: f32 = Link::LEVEL;
/// How long a fence holds if no intent ever agrees (a client that does not take corrections is not shut out for good).
pub(super) const FENCE_TIMEOUT: Duration = Duration::from_secs(2);

/// What a link left the server holding a player at, until its client takes it.
#[derive(Debug, Clone, Copy)]
pub(super) struct Fence {
    to: Pos,
    until: Instant,
}

/// Is the intent to `to` held back by the player's `fence`? `Some(position)`: yes, answer with that position. The
/// fence is lifted by an intent that agrees with it, or by time.
pub(super) fn fenced(fence: &mut Option<Fence>, to: Pos, now: Instant) -> Option<Pos> {
    let held = (*fence)?;
    if now >= held.until || (to.dist_xy(held.to) <= FENCE_XY && (to.z - held.to.z).abs() <= FENCE_Z) {
        *fence = None;
        return None;
    }
    Some(held.to)
}

/// What a press came to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PressOutcome {
    /// Nothing the player could notice happened, or only a story step (which tells the player itself).
    Nothing,
    /// A link moved the player to `corrected`: tell its client with `MoveRejected`.
    Relocated { corrected: Pos },
    /// The entity is no longer in the world.
    Gone,
}

impl World {
    /// `entity` has been placed where it stands now (it joined, arrived through an exit): the links within reach start
    /// disarmed and nothing is fenced.
    pub(super) fn place_links(&mut self, entity: EntityId) {
        let Some(p) = self.players.get_mut(&entity) else { return };
        p.fence = None;
        p.arming = match self.areas.get(&p.area) {
            Some(area) => LinkArming::placed_at(area.links(), p.snapshot.pos),
            None => LinkArming::default(),
        };
    }

    /// The accepted move `from -> to` of `entity`: fire the walk-on link it touches while armed, if one (module
    /// docs). Returns where the player was put. A player that the same hop put into a battle (a story point that starts
    /// one) is frozen where it is: no link moves it.
    pub(super) fn walk_links(&mut self, entity: EntityId, from: Pos, to: Pos, now: Instant) -> Option<Pos> {
        let fired = {
            let p = self.players.get_mut(&entity)?;
            if p.battle.is_some() || p.transit.is_some() {
                return None;
            }
            let area = self.areas.get(&p.area)?;
            p.arming.step(area.links(), from, to)?
        };
        self.relocate(entity, fired, now)
    }

    /// Put `entity` where link `index` of its area leads: on the destination's own surface, fenced, with every link
    /// within reach of the new place disarmed. Returns the position, or `None` when the destination cannot be placed.
    fn relocate(&mut self, entity: EntityId, index: usize, now: Instant) -> Option<Pos> {
        let (dest, area_key, label) = {
            let p = self.players.get(&entity)?;
            let area = self.areas.get(&p.area)?;
            let link = area.links().get(index)?;
            let dest = area.runtime().place_arrival(link.to, link.to_hint);
            if !dest.is_finite() {
                return None;
            }
            (dest, p.area.clone(), link.label.clone())
        };
        let p = self.players.get_mut(&entity)?;
        p.snapshot.pos = dest;
        p.dirty = true;
        p.fence = Some(Fence { to: dest, until: now + FENCE_TIMEOUT });
        if let Some(area) = self.areas.get(&area_key) {
            p.arming = LinkArming::placed_at(area.links(), dest);
        }
        let game_area = self.areas.get(&area_key).map_or(area_key, |a| a.id().to_string());
        self.logs.info(format!(
            "player used a link entity={} area={game_area} link={index} ({label}) to=({:.0},{:.0})",
            entity.0, dest.x, dest.y
        ));
        // The place it is put in may be a story region of its own.
        self.check_story(entity, dest, dest, now);
        Some(dest)
    }

    /// `ClientMsg::Interact`: the player pressed OK (module docs).
    pub fn press(&mut self, entity: EntityId, npc: Option<u32>, now: Instant) -> PressOutcome {
        let (pos, area_key, phase) = {
            let Some(p) = self.players.get(&entity) else { return PressOutcome::Gone };
            // (a player held in a story scene presses nothing: see `scene`)
            if p.battle.is_some() || p.transit.is_some() || p.scene.is_some() || p.fence.is_some_and(|f| now < f.until) {
                return PressOutcome::Nothing;
            }
            (p.snapshot.pos, p.area.clone(), p.placement.as_ref().map(|p| p.key.phase.clone()).unwrap_or_default())
        };
        // OK next to a scene's trigger starts the scene (see `scene`): the trigger is no character of the area's own.
        if let Some(npc) = npc.filter(|n| super::scene::scene_index(*n).is_some()) {
            self.press_scene(entity, npc, now);
            return PressOutcome::Nothing;
        }
        let (points, link) = {
            let Some(area) = self.areas.get(&area_key) else { return PressOutcome::Gone };
            // A character the area does not have is no character addressed: the press is where the player stands.
            let addressed = npc.and_then(|id| area.npc_position(id, &phase));
            let talking = addressed.is_some();
            let points: Vec<u32> = area
                .story_points_in_phase(&phase)
                .iter()
                .filter(|point| matches!(point.trigger, StoryTrigger::Talk { .. }) == talking && point.is_pressed_at(pos, addressed))
                .map(|point| point.id)
                .collect();
            let link = if talking { None } else { area.links().iter().position(|l| l.needs_ok && l.is_touched_at(pos)) };
            (points, link)
        };
        // The story: each point is judged on the story as it was before the press, and the first that applies is the
        // press's (the lift's ride down and ride up exclude each other).
        for point in points {
            if self.reach_story_point(entity, point, Some(now), now, false) != super::story::Reached::Nothing {
                return PressOutcome::Nothing;
            }
        }
        match link.and_then(|index| self.relocate(entity, index, now)) {
            Some(corrected) => PressOutcome::Relocated { corrected },
            None => PressOutcome::Nothing,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use fmv_adapter_api::GameAdapter;
    use fmv_core::{AccountId, AnimState, AvatarAppearance};
    use fmv_fixture::rooms::{RoomsAdapter, HALL, LINK_A_TO, LINK_B_TO, LINK_C_TO, PRESS_REGION, STORY_ADVANCED, YARD_NPC_POS};
    use fmv_party::PartyConfig;
    use fmv_protocol::ServerMsg;
    use tokio::sync::{mpsc, watch};

    use super::super::{MoveOutcome, NewPlayer, Outbound};
    use super::*;
    use crate::saver::{Msg, SaverHandle};

    struct Seat {
        entity: EntityId,
        rx: mpsc::Receiver<Outbound>,
    }

    impl Seat {
        fn drain(&mut self) -> Vec<ServerMsg> {
            let mut out = Vec::new();
            while let Ok(m) = self.rx.try_recv() {
                out.push((*m).clone());
            }
            out
        }
    }

    /// The rooms with the things that wait for OK and the lines that move a player, and a saver queue to read.
    fn world() -> (World, mpsc::UnboundedReceiver<Msg>) {
        let adapter: Arc<dyn GameAdapter> = Arc::new(RoomsAdapter::with_interactions());
        let mut w = World::with_party_config(adapter.load_area(HALL).unwrap(), PartyConfig::default()).unwrap();
        w.set_area_source(adapter);
        let (handle, rx) = SaverHandle::channel();
        w.set_progress_saver(handle);
        (w, rx)
    }

    fn seat(w: &mut World, name: &str, now: Instant) -> Seat {
        let (session, entity) = w.allocate_ids();
        let (tx, rx) = mpsc::channel(256);
        let close = Arc::new(watch::channel(false).0);
        let avatar = AvatarAppearance { display_name: name.into(), tint: [1, 2, 3], appearance_ref: None };
        let account = AccountId(entity.0);
        w.join(NewPlayer { session, entity, account, avatar, progression: Vec::new(), progress: Default::default(), tx, close }, now).expect("joins");
        Seat { entity, rx }
    }

    /// Hop to (x, y) after the budget had 700 ms to refill.
    fn go(w: &mut World, s: &Seat, x: f32, y: f32, now: &mut Instant) -> MoveOutcome {
        *now += Duration::from_millis(700);
        w.apply_move(s.entity, Pos::new(x, y, 0.0), 0.0, AnimState::Walk, *now)
    }

    fn at(w: &World, s: &Seat) -> Pos {
        w.players.get(&s.entity).unwrap().snapshot.pos
    }

    fn progression(w: &World, s: &Seat) -> Vec<u8> {
        w.players.get(&s.entity).unwrap().progression.clone()
    }

    fn log_text(w: &mut World) -> String {
        w.take_logs().iter().map(|l| l.text().to_string()).collect::<Vec<_>>().join("\n")
    }

    fn saved_stories(rx: &mut mpsc::UnboundedReceiver<Msg>) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            if let Msg::Story(job) = msg {
                out.push(job.progress);
            }
        }
        out
    }

    fn pos(xy: (f32, f32)) -> Pos {
        Pos::new(xy.0, xy.1, 0.0)
    }

    #[test]
    fn a_walk_on_link_puts_the_player_across_the_hall_and_the_line_it_lands_beside_does_not_send_it_back() {
        let t0 = Instant::now();
        let (mut w, _saved) = world();
        let mut a = seat(&mut w, "A", t0);
        a.drain();
        w.take_logs();
        let mut now = t0;

        // from the spawn at (-100, 0) toward link A (the line x = -150): 30 units from it is not touching it
        assert_eq!(go(&mut w, &a, -120.0, 0.0, &mut now), MoveOutcome::Applied);
        assert_eq!(go(&mut w, &a, -140.0, 0.0, &mut now), MoveOutcome::Relocated { corrected: pos(LINK_A_TO) });
        assert_eq!(at(&w, &a), pos(LINK_A_TO), "the server holds the player at the destination");
        let log = log_text(&mut w);
        assert!(log.contains(&format!("player used a link entity={} area=Fixture/hall link=0 (jump) to=(150,0)", a.entity.0)), "{log}");
        // the others are told with the next tick: the entity changed
        assert!(w.players.get(&a.entity).unwrap().dirty);

        // the client takes it (an intent that agrees) and walks on: the destination is 20 units from link B's line, so B
        // is disarmed, and walking back over it fires nothing
        assert_eq!(go(&mut w, &a, 140.0, 0.0, &mut now), MoveOutcome::Applied);
        assert_eq!(go(&mut w, &a, 100.0, 0.0, &mut now), MoveOutcome::Applied);
        assert_eq!(at(&w, &a).x, 100.0, "over the line of B, not sent back");
        // out of reach of the line arms it (here: 40 units away); walking up to it again fires it
        assert_eq!(go(&mut w, &a, 90.0, 0.0, &mut now), MoveOutcome::Applied);
        assert_eq!(go(&mut w, &a, 120.0, 0.0, &mut now), MoveOutcome::Relocated { corrected: pos(LINK_B_TO) });
        assert_eq!(at(&w, &a), pos(LINK_B_TO));
    }

    #[test]
    fn after_a_link_the_player_is_fenced_until_its_client_takes_the_new_position() {
        let t0 = Instant::now();
        let (mut w, _saved) = world();
        let a = seat(&mut w, "A", t0);
        let mut now = t0;
        assert_eq!(go(&mut w, &a, -140.0, 0.0, &mut now), MoveOutcome::Relocated { corrected: pos(LINK_A_TO) });
        // intents the client sent from where it still stands (the destination is 200 and more units from where they
        // are) are judged from the old place by nobody: each is answered with the destination and moves nothing
        for y in [200.0, 210.0] {
            assert_eq!(go(&mut w, &a, 150.0, y, &mut now), MoveOutcome::Relocated { corrected: pos(LINK_A_TO) });
            assert_eq!(at(&w, &a), pos(LINK_A_TO));
        }
        // the intent that agrees (the client's resync after it took the correction) lifts the fence, and is judged:
        // from then on the player moves like any other
        assert_eq!(go(&mut w, &a, 150.0, 0.0, &mut now), MoveOutcome::Applied);
        assert_eq!(go(&mut w, &a, 150.0, 30.0, &mut now), MoveOutcome::Applied);
        assert_eq!(at(&w, &a), Pos::new(150.0, 30.0, 0.0));
    }

    #[test]
    fn a_client_that_never_takes_the_correction_is_not_fenced_for_good() {
        let t0 = Instant::now();
        let (mut w, _saved) = world();
        let a = seat(&mut w, "A", t0);
        let mut now = t0;
        assert_eq!(go(&mut w, &a, -140.0, 0.0, &mut now), MoveOutcome::Relocated { corrected: pos(LINK_A_TO) });
        // 700 ms per hop: the link was walked at 0.7 s, the fence (2 s) holds the hops at 1.4 s and 2.1 s and is gone
        // at 2.8 s, which is an ordinary move
        let outcomes: Vec<bool> = (0..4).map(|_| matches!(go(&mut w, &a, 150.0, 200.0, &mut now), MoveOutcome::Relocated { .. })).collect();
        assert_eq!(outcomes, [true, true, false, false]);
        assert_eq!(at(&w, &a), Pos::new(150.0, 200.0, 0.0));
    }

    #[test]
    fn the_fence_judges_the_level_and_the_distance_of_an_intent_and_time() {
        let t0 = Instant::now();
        let held = || Some(Fence { to: Pos::new(100.0, 100.0, 1321.0), until: t0 + FENCE_TIMEOUT });
        // a ladder's destination: an intent still on the level below, a few units away, is not the client's resync
        let mut fence = held();
        assert_eq!(fenced(&mut fence, Pos::new(110.0, 100.0, 861.0), t0), Some(Pos::new(100.0, 100.0, 1321.0)));
        assert!(fence.is_some());
        // one on the new level and near it is
        assert_eq!(fenced(&mut fence, Pos::new(110.0, 100.0, 1321.0), t0), None);
        assert!(fence.is_none());
        // one at its level but far away is not
        let mut fence = held();
        assert!(fenced(&mut fence, Pos::new(100.0 + FENCE_XY + 1.0, 100.0, 1321.0), t0).is_some());
        // time lifts it
        assert_eq!(fenced(&mut fence, Pos::new(900.0, 100.0, 0.0), t0 + FENCE_TIMEOUT), None);
        assert!(fence.is_none());
        // no fence, no answer
        assert_eq!(fenced(&mut None, Pos::default(), t0), None);
    }

    #[test]
    fn a_link_that_needs_ok_is_not_fired_by_walking_and_is_by_a_press_on_its_line() {
        let t0 = Instant::now();
        let (mut w, _saved) = world();
        let mut a = seat(&mut w, "A", t0);
        a.drain();
        w.take_logs();
        let mut now = t0;
        // over the line of C (y = 100) and back: nothing
        assert_eq!(go(&mut w, &a, -50.0, 50.0, &mut now), MoveOutcome::Applied);
        assert_eq!(go(&mut w, &a, 0.0, 80.0, &mut now), MoveOutcome::Applied);
        assert_eq!(go(&mut w, &a, 0.0, 120.0, &mut now), MoveOutcome::Applied);
        assert_eq!(go(&mut w, &a, 0.0, 95.0, &mut now), MoveOutcome::Applied);
        assert_eq!(at(&w, &a), Pos::new(0.0, 95.0, 0.0));
        // a press away from every line does nothing
        assert_eq!(go(&mut w, &a, 0.0, 60.0, &mut now), MoveOutcome::Applied);
        assert_eq!(w.press(a.entity, None, now), PressOutcome::Nothing);
        assert_eq!(at(&w, &a).y, 60.0);
        // touching the line and pressing puts the player where the ladder leads
        assert_eq!(go(&mut w, &a, 0.0, 95.0, &mut now), MoveOutcome::Applied);
        w.players.get_mut(&a.entity).unwrap().placement.as_mut().unwrap().story_advance_allowed = false;
        assert_eq!(w.press(a.entity, None, now), PressOutcome::Relocated { corrected: pos(LINK_C_TO) });
        assert_eq!(at(&w, &a), pos(LINK_C_TO));
        assert!(log_text(&mut w).contains("link=2 (ladder) to=(0,-150)"));
        // the client has not taken it yet: a second press is no press (and a hop from the old place is answered)
        assert_eq!(w.press(a.entity, None, now), PressOutcome::Nothing);
        assert!(matches!(go(&mut w, &a, 0.0, 100.0, &mut now), MoveOutcome::Relocated { .. }));
    }

    #[test]
    fn pressing_ok_in_a_press_region_advances_the_story_once_and_walking_through_does_not() {
        let t0 = Instant::now();
        let (mut w, mut saved) = world();
        let mut a = seat(&mut w, "A", t0);
        a.drain();
        w.take_logs();
        let mut now = t0;
        assert_eq!(PRESS_REGION[0], (150.0, 100.0));
        for (x, y) in [(0.0, 60.0), (100.0, 100.0), (200.0, 130.0), (240.0, 150.0)] {
            assert_eq!(go(&mut w, &a, x, y, &mut now), MoveOutcome::Applied);
        }
        assert!(progression(&w, &a).is_empty(), "standing in the region is not pressing OK");
        // a press outside the region reaches nothing
        assert_eq!(go(&mut w, &a, 270.0, 150.0, &mut now), MoveOutcome::Applied);
        assert_eq!(w.press(a.entity, None, now), PressOutcome::Nothing);
        assert!(progression(&w, &a).is_empty());
        // inside it, it is the step
        assert_eq!(go(&mut w, &a, 240.0, 150.0, &mut now), MoveOutcome::Applied);
        assert_eq!(w.press(a.entity, None, now), PressOutcome::Nothing, "the step tells the player itself");
        assert_eq!(progression(&w, &a), STORY_ADVANCED);
        let log = log_text(&mut w);
        assert!(log.contains(&format!("story advanced account={} entity={} area=Fixture/hall point=1 moment=0->1", a.entity.0, a.entity.0)), "{log}");
        assert_eq!(saved_stories(&mut saved), [STORY_ADVANCED.to_vec()], "the step was queued for the saver");
        assert!(a.drain().iter().any(|m| matches!(m, ServerMsg::StoryUpdate { advance: Some(_), .. })));
        // once
        assert_eq!(w.press(a.entity, None, now), PressOutcome::Nothing);
        assert!(!log_text(&mut w).contains("story advanced"));
    }

    /// Walk `s` to the yard through the hall's gateway and let it in.
    fn arrive_in_the_yard(w: &mut World, s: &mut Seat, now: &mut Instant) {
        assert_eq!(go(w, s, 100.0, 150.0, now), MoveOutcome::Applied);
        assert_eq!(go(w, s, 350.0, 150.0, now), MoveOutcome::Applied);
        let offer = s.drain().into_iter().find(|m| matches!(m, ServerMsg::AreaChange { .. })).expect("an offer");
        let ServerMsg::AreaChange { area, fingerprint, provenance, .. } = offer else { unreachable!() };
        assert_eq!(w.join_area(s.entity, &area, &fingerprint, &provenance, *now), crate::world::TravelReply::Done);
    }

    #[test]
    fn pressing_ok_next_to_the_character_a_talk_point_belongs_to_is_the_talk_and_nothing_else_is() {
        let t0 = Instant::now();
        let (mut w, mut saved) = world();
        let mut a = seat(&mut w, "A", t0);
        let mut now = t0;
        arrive_in_the_yard(&mut w, &mut a, &mut now);
        a.drain();
        w.take_logs();
        assert_eq!(YARD_NPC_POS, (200.0, 100.0));
        // far from the guard (the yard's arrival is 360 units from it): the press reaches nobody
        assert_eq!(w.press(a.entity, Some(0), now), PressOutcome::Nothing);
        assert!(progression(&w, &a).is_empty());
        // walking up to it is not talking to it
        assert_eq!(go(&mut w, &a, 150.0, 100.0, &mut now), MoveOutcome::Applied);
        assert!(progression(&w, &a).is_empty());
        // a press naming a character the area does not have is a press where the player stands: nothing there
        assert_eq!(w.press(a.entity, Some(7), now), PressOutcome::Nothing);
        assert!(progression(&w, &a).is_empty());
        // Address what this player's effective phase shows, not the base cast or personal story.
        w.players.get_mut(&a.entity).unwrap().placement.as_mut().unwrap().key.phase = fmv_core::PhaseKey(fmv_fixture::rooms::STORY_ADVANCED_PHASE.to_vec());
        assert_eq!(w.press(a.entity, Some(0), now), PressOutcome::Nothing);
        assert!(progression(&w, &a).is_empty(), "a hidden character cannot be addressed");
        w.players.get_mut(&a.entity).unwrap().placement.as_mut().unwrap().key.phase = fmv_core::PhaseKey(fmv_fixture::rooms::STORY_MOVED_PHASE.to_vec());
        assert_eq!(w.press(a.entity, Some(0), now), PressOutcome::Nothing);
        assert!(progression(&w, &a).is_empty(), "the old talk location is no longer the target");
        let player = w.players.get_mut(&a.entity).unwrap();
        player.snapshot.pos = Pos::new(-50.0, -100.0, Link::LEVEL + 1.0);
        assert_eq!(w.press(a.entity, Some(0), now), PressOutcome::Nothing);
        assert!(progression(&w, &a).is_empty(), "wrong floor");
        let player = w.players.get_mut(&a.entity).unwrap();
        player.snapshot.pos.z = 0.0;
        player.placement.as_mut().unwrap().story_advance_allowed = false;
        assert_eq!(w.press(a.entity, Some(0), now), PressOutcome::Nothing);
        assert!(progression(&w, &a).is_empty(), "party phase does not grant personal story permission");
        assert!(saved_stories(&mut saved).is_empty());
        w.players.get_mut(&a.entity).unwrap().placement.as_mut().unwrap().story_advance_allowed = true;
        // The same NPC/point IDs at their new position, with permission, pay the existing step once.
        assert_eq!(w.press(a.entity, Some(0), now), PressOutcome::Nothing);
        assert_eq!(progression(&w, &a), STORY_ADVANCED);
        assert_eq!(saved_stories(&mut saved), [STORY_ADVANCED.to_vec()]);
        let log = log_text(&mut w);
        assert!(log.contains("area=Fixture/yard point=0 moment=0->1"), "{log}");
        assert!(a.drain().iter().any(|msg| matches!(msg, ServerMsg::AreaSnapshot { npcs, .. } if npcs.is_empty())), "the guard leaves immediately in the new phase");
        assert_eq!(w.press(a.entity, Some(0), now), PressOutcome::Nothing);
        assert!(saved_stories(&mut saved).is_empty(), "no repeated save");
    }

    #[test]
    fn a_press_whose_step_is_not_forward_is_applied_as_the_step_says_and_walking_into_its_region_does_not_press() {
        use fmv_fixture::rooms::TOGGLE_REGION;
        let t0 = Instant::now();
        let (mut w, mut saved) = world();
        let mut a = seat(&mut w, "A", t0);
        a.drain();
        w.take_logs();
        let mut now = t0;
        // the press region first: a new account's story moves
        for (x, y) in [(0.0, 60.0), (100.0, 100.0), (200.0, 130.0), (240.0, 150.0)] {
            assert_eq!(go(&mut w, &a, x, y, &mut now), MoveOutcome::Applied);
        }
        assert_eq!(w.press(a.entity, None, now), PressOutcome::Nothing);
        assert_eq!(progression(&w, &a), STORY_ADVANCED);
        // walking into the toggle region is not pressing it
        assert_eq!(TOGGLE_REGION[0], (150.0, 170.0));
        assert_eq!(go(&mut w, &a, 240.0, 200.0, &mut now), MoveOutcome::Applied);
        assert_eq!(progression(&w, &a), STORY_ADVANCED, "standing in the toggle region is not pressing OK");
        // the press is the ride back: the story the world holds is what the step gives (a new account's), the player is told
        // with the step, and the saver is handed it (what the store keeps of it is the adapter's merge)
        a.drain();
        w.take_logs();
        assert_eq!(w.press(a.entity, None, now), PressOutcome::Nothing);
        assert!(progression(&w, &a).is_empty());
        assert!(a.drain().iter().any(|m| matches!(m, ServerMsg::StoryUpdate { progress, advance: Some(_) } if progress.is_empty())));
        assert!(log_text(&mut w).contains("point=4 moment=1->0"));
        assert_eq!(saved_stories(&mut saved), [STORY_ADVANCED.to_vec(), Vec::new()]);
        // pressing again: a new account's story has nowhere to go back to
        assert_eq!(w.press(a.entity, None, now), PressOutcome::Nothing);
        assert!(progression(&w, &a).is_empty());
        assert!(saved_stories(&mut saved).is_empty(), "nothing more was saved");
    }

    #[test]
    fn a_player_in_a_battle_is_moved_by_no_link_and_presses_nothing() {
        let t0 = Instant::now();
        let (mut w, _saved) = world();
        let a = seat(&mut w, "A", t0);
        // the hop of a story point that starts a battle and touches a walk-on link in the same step leaves the player in
        // the battle: the link does not move it, and does not fence it
        w.players.get_mut(&a.entity).unwrap().battle = Some(fmv_protocol::BattleId(1));
        assert_eq!(w.walk_links(a.entity, pos((-100.0, 0.0)), pos((-140.0, 0.0)), t0), None);
        assert_eq!(at(&w, &a), pos((-100.0, 0.0)));
        assert!(w.players.get(&a.entity).unwrap().fence.is_none());
        // and OK at a ladder in a battle is not the ladder
        w.players.get_mut(&a.entity).unwrap().snapshot.pos = Pos::new(0.0, 95.0, 0.0);
        assert_eq!(w.press(a.entity, None, t0), PressOutcome::Nothing);
        assert_eq!(at(&w, &a), Pos::new(0.0, 95.0, 0.0));
        // out of the battle the same press is the ladder
        w.players.get_mut(&a.entity).unwrap().battle = None;
        assert_eq!(w.press(a.entity, None, t0), PressOutcome::Relocated { corrected: pos(LINK_C_TO) });
    }

    #[test]
    fn a_press_of_somebody_who_is_not_in_the_world_is_gone_and_one_that_reaches_nothing_is_nothing() {
        let t0 = Instant::now();
        let (mut w, _saved) = world();
        let a = seat(&mut w, "A", t0);
        assert_eq!(w.press(EntityId(999), None, t0), PressOutcome::Gone);
        assert_eq!(w.press(a.entity, None, t0), PressOutcome::Nothing);
        assert_eq!(w.press(a.entity, Some(3), t0), PressOutcome::Nothing);
    }
}
