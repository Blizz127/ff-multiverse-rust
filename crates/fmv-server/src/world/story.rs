//! The story in the world: what happens when a player's validated position reaches a story point.
//!
//! An account's story progression (`NewPlayer::progression`, mirrored in `Player::progression`) is
//! the game's own blob; the world never reads it. What it does is ask the area the player stands in.
//! After every move the server accepts, it tests the move against the area's story points
//! (`AreaRuntime::story_points`, decoded once when the area is loaded; a chord that crosses the region
//! counts, not only where it ends) and, for each point touched, asks the area what reaching it does for
//! THIS progression (`AreaRuntime::story_advance`). A step is applied as one unit, in this order:
//!
//! 1. the player's mirror takes the new progression (the story only moves forward: the adapter decides);
//! 2. the server logs ONE line (the one `scripts/phase-smoke.sh` and a headless bot's caller look for):
//!    `story advanced account=<n> entity=<n> area=<AreaId> point=<id> <summary>`, for FF7
//!    `story advanced account=3 entity=7 area=Ff7/md1stin point=0 moment=1->6`;
//! 3. the save is queued for the saver (`crate::saver`), which writes it off the world lock with the
//!    adapter's own merge, so a stale save can never take a story back (a server without a saver, like
//!    a unit test of the world, saves nothing);
//! 4. the player is told with `ServerMsg::StoryUpdate`;
//! 5. the placement of the player and of every member of its party is refreshed (`World::settle`): a
//!    changed phase moves the player to another instance of the same area, and when the player leads a
//!    party its members follow (they keep their own progression).
//!
//! A party member whose own phase is not the one that chose the instance (`story_advance_allowed` is
//! false in its placement) cannot advance the story there: the instance runs the leader's phase and
//! the member's story waits until the member stands in a place of its own phase.

use std::collections::BTreeSet;
use std::sync::Arc;

use fmv_core::{EntityId, Pos};
use fmv_protocol::{ServerMsg, StoryAdvance};

use super::{is_full, World};

impl World {
    /// The accepted move `from -> to` of `entity`: reach every story point of its area that it touches
    /// and advance its story where that applies (module docs). Does nothing for a player that is in a
    /// battle or in transit, or whose placement does not let it advance the story.
    pub(super) fn check_story(&mut self, entity: EntityId, from: Pos, to: Pos) {
        let touched: Vec<u32> = {
            let Some(player) = self.players.get(&entity).filter(|p| p.battle.is_none() && p.transit.is_none()) else { return };
            if player.placement.as_ref().is_some_and(|p| !p.story_advance_allowed) {
                return;
            }
            let Some(area) = self.areas.get(&player.area) else { return };
            area.story_points().iter().filter(|point| point.is_hit_by(from, to)).map(|point| point.id).collect()
        };
        for point in touched {
            self.reach_story_point(entity, point);
        }
    }

    /// `entity` reached story point `point` of its area: advance its story if the point applies to it.
    fn reach_story_point(&mut self, entity: EntityId, point: u32) {
        let step = {
            let Some(player) = self.players.get(&entity) else { return };
            let Some(area) = self.areas.get(&player.area) else { return };
            area.runtime().story_advance(&player.progression, point)
        };
        let Some(step) = step else { return };
        let Some(player) = self.players.get_mut(&entity) else { return };
        let (account, area_id) = (player.account, player.area.clone());
        player.progression = step.progress.clone();
        let game_area = self.areas.get(&area_id).map(|a| a.id().to_string()).unwrap_or(area_id);
        self.logs.info(format!(
            "story advanced account={} entity={} area={game_area} point={point} {}",
            account.0, entity.0, step.summary
        ));
        if !self.saver.as_ref().is_none_or(|saver| saver.save_story(account, step.progress.clone())) {
            self.logs.warn(format!("story progress NOT SAVED (the saver has stopped; the player has it, the store does not) account={}", account.0));
        }

        let mut failed = Vec::new();
        self.tell_story(entity, Some(StoryAdvance { point, summary: step.summary }), &mut failed);
        // The player's own placement may change (its phase did), and so may every party member's: the
        // instance of a party is the leader's phase.
        let mut accounts = BTreeSet::from([account]);
        if let Some(party) = self.parties.party_of(account) {
            accounts.extend(party.members.iter().copied());
        }
        self.refresh_placements(&accounts, &mut failed);
        self.drop_failed(failed);
    }

    /// Tell `entity` its story progress (`StoryUpdate`): the whole progression as the server holds it,
    /// and `advance` when this follows a step.
    pub(super) fn tell_story(&self, entity: EntityId, advance: Option<StoryAdvance>, failed: &mut Vec<(EntityId, bool)>) {
        let Some(player) = self.players.get(&entity) else { return };
        let msg = ServerMsg::StoryUpdate { progress: player.progression.clone(), advance };
        if let Err(e) = player.tx.try_send(Arc::new(msg)) {
            failed.push((entity, is_full(&e)));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use fmv_adapter_api::GameAdapter;
    use fmv_core::{AccountId, AnimState, AvatarAppearance, PhaseKey};
    use fmv_fixture::rooms::{RoomsAdapter, HALL, STORY_ADVANCED, STORY_ADVANCED_PHASE, YARD};
    use fmv_party::PartyConfig;
    use fmv_protocol::{InviteId, Placement};
    use tokio::sync::{mpsc, watch};

    use super::super::{MoveOutcome, NewPlayer, Outbound, PartyOutcome};
    use super::*;
    use crate::saver::{Msg, SaverHandle};

    struct Seat {
        entity: EntityId,
        account: fmv_core::AccountId,
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

    /// A world over the story rooms, with a saver queue the test can read.
    fn story_world() -> (World, mpsc::UnboundedReceiver<Msg>) {
        let adapter: Arc<dyn GameAdapter> = Arc::new(RoomsAdapter::with_story());
        let mut w = World::with_party_config(adapter.load_area(HALL).unwrap(), PartyConfig::default()).unwrap();
        w.set_area_source(adapter);
        let (handle, rx) = SaverHandle::channel();
        w.set_progress_saver(handle);
        (w, rx)
    }

    /// A player with the given stored progression (empty: a new account); the account is its entity's number.
    fn seat(w: &mut World, name: &str, progression: &[u8], now: Instant) -> Seat {
        let (session, entity) = w.allocate_ids();
        let (tx, rx) = mpsc::channel(256);
        let close = Arc::new(watch::channel(false).0);
        let avatar = AvatarAppearance { display_name: name.into(), tint: [1, 2, 3], appearance_ref: None };
        let account = AccountId(entity.0);
        w.join(NewPlayer { session, entity, account, avatar, progression: progression.to_vec(), progress: Default::default(), tx, close }, now)
            .expect("joins");
        Seat { entity, account, rx }
    }

    /// Hop to (x, y) after the budget had 700 ms to refill.
    fn go(w: &mut World, s: &Seat, x: f32, y: f32, now: &mut Instant) -> MoveOutcome {
        *now += Duration::from_millis(700);
        w.apply_move(s.entity, Pos::new(x, y, 0.0), 0.0, AnimState::Walk, *now)
    }

    fn progression_of(w: &World, s: &Seat) -> Vec<u8> {
        w.players.get(&s.entity).unwrap().progression.clone()
    }

    fn placements(msgs: &[ServerMsg]) -> Vec<Placement> {
        msgs.iter()
            .filter_map(|m| match m {
                ServerMsg::InstancePlacement { placement } => Some(placement.clone()),
                _ => None,
            })
            .collect()
    }

    /// `(entities, npc count)` of every snapshot.
    fn snapshots(msgs: &[ServerMsg]) -> Vec<(Vec<EntityId>, usize)> {
        msgs.iter()
            .filter_map(|m| match m {
                ServerMsg::AreaSnapshot { entities, npcs, .. } => Some((entities.iter().map(|e| e.id).collect(), npcs.len())),
                _ => None,
            })
            .collect()
    }

    fn story_updates(msgs: &[ServerMsg]) -> Vec<(Vec<u8>, bool)> {
        msgs.iter()
            .filter_map(|m| match m {
                ServerMsg::StoryUpdate { progress, advance } => Some((progress.clone(), advance.is_some())),
                _ => None,
            })
            .collect()
    }

    fn saved_stories(rx: &mut mpsc::UnboundedReceiver<Msg>) -> Vec<(AccountId, Vec<u8>)> {
        let mut out = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            if let Msg::Story(job) = msg {
                out.push((job.account, job.progress));
            }
        }
        out
    }

    fn log_text(w: &mut World) -> String {
        w.take_logs().iter().map(|l| l.text().to_string()).collect::<Vec<_>>().join("\n")
    }

    fn advanced_phase() -> PhaseKey {
        PhaseKey(STORY_ADVANCED_PHASE.to_vec())
    }

    fn leave_of(msgs: &[ServerMsg], entity: EntityId) -> bool {
        msgs.iter().any(|m| matches!(m, ServerMsg::PlayerLeft { entity: e } if *e == entity))
    }

    fn join_of(msgs: &[ServerMsg], entity: EntityId) -> bool {
        msgs.iter().any(|m| matches!(m, ServerMsg::PlayerJoined { snapshot } if snapshot.id == entity))
    }

    #[test]
    fn every_new_account_shares_one_instance_and_gets_exactly_the_messages_it_always_got() {
        let t0 = Instant::now();
        let (mut w, mut saved) = story_world();
        let mut a = seat(&mut w, "A", &[], t0);
        let mut b = seat(&mut w, "B", &[], t0);
        assert_eq!(w.instance_of(a.account), w.instance_of(b.account));
        assert_eq!(w.instances.len(), 1);
        // A's join: its snapshot, its placement; then B's arrival; nothing about a story.
        let a_msgs = a.drain();
        assert!(
            matches!(&a_msgs[..], [ServerMsg::AreaSnapshot { .. }, ServerMsg::InstancePlacement { .. }, ServerMsg::PlayerJoined { .. }]),
            "{a_msgs:?}"
        );
        let b_msgs = b.drain();
        assert!(matches!(&b_msgs[..], [ServerMsg::AreaSnapshot { .. }, ServerMsg::InstancePlacement { .. }]), "{b_msgs:?}");
        let p = &placements(&b_msgs)[0];
        assert_eq!((p.key.phase.clone(), p.party, p.story_advance_allowed), (PhaseKey::default(), None, true), "the empty phase, as always");
        assert!(saved_stories(&mut saved).is_empty());
    }

    #[test]
    fn walking_into_the_story_region_advances_the_player_and_parts_it_from_those_who_did_not() {
        let t0 = Instant::now();
        let (mut w, mut saved) = story_world();
        let mut a = seat(&mut w, "A", &[], t0);
        let mut b = seat(&mut w, "B", &[], t0);
        a.drain();
        b.drain();
        w.take_logs();
        let mut now = t0;

        // A walks into the region (x 150..250, y -50..50) from its spawn at (-100, 0).
        assert_eq!(go(&mut w, &a, 100.0, 0.0, &mut now), MoveOutcome::Applied);
        assert_eq!(go(&mut w, &a, 200.0, 0.0, &mut now), MoveOutcome::Applied);

        // The story moved: mirror, log line, saver queue, the player's own messages.
        assert_eq!(progression_of(&w, &a), STORY_ADVANCED);
        let log = log_text(&mut w);
        let line = format!("story advanced account={} entity={} area=Fixture/hall point=0 moment=0->1", a.account.0, a.entity.0);
        assert!(log.contains(&line), "{log}");
        assert_eq!(saved_stories(&mut saved), [(a.account, STORY_ADVANCED.to_vec())], "queued once, for the saver");
        let a_msgs = a.drain();
        match &a_msgs[..] {
            [ServerMsg::StoryUpdate { progress, advance: Some(advance) }, ServerMsg::AreaSnapshot { area, entities, npcs, .. }, ServerMsg::InstancePlacement { placement }] =>
            {
                assert_eq!((progress.as_slice(), advance.point, advance.summary.as_str()), (STORY_ADVANCED, 0, "moment=0->1"));
                assert_eq!(area.key, HALL, "the same area");
                assert_eq!(entities.iter().map(|e| e.id).collect::<Vec<_>>(), [a.entity], "A is alone in its new instance");
                assert!(npcs.is_empty());
                assert_eq!((placement.key.phase.clone(), placement.party, placement.story_advance_allowed), (advanced_phase(), None, true));
            }
            other => panic!("{other:?}"),
        }
        // B, left in the old instance, hears A leave, and from then on neither sees the other.
        let b_msgs = b.drain();
        assert!(matches!(&b_msgs[..], [ServerMsg::PlayerLeft { entity }] if *entity == a.entity), "{b_msgs:?}");
        assert_ne!(w.instance_of(a.account), w.instance_of(b.account));
        assert_eq!(w.instances.len(), 2);
        assert_eq!(go(&mut w, &a, 210.0, 10.0, &mut now), MoveOutcome::Applied);
        assert_eq!(go(&mut w, &b, 0.0, 50.0, &mut now), MoveOutcome::Applied);
        w.advance_tick();
        let (a_msgs, b_msgs) = (a.drain(), b.drain());
        assert!(matches!(&a_msgs[..], [ServerMsg::EntityUpdate { entities, .. }] if entities.iter().all(|e| e.id == a.entity)), "{a_msgs:?}");
        assert!(matches!(&b_msgs[..], [ServerMsg::EntityUpdate { entities, .. }] if entities.iter().all(|e| e.id == b.entity)), "{b_msgs:?}");
        // The step is once: standing in the region again changes nothing and says nothing.
        assert_eq!(go(&mut w, &a, 190.0, 0.0, &mut now), MoveOutcome::Applied);
        assert!(saved_stories(&mut saved).is_empty() && story_updates(&a.drain()).is_empty());
        // B has not advanced: its progression is untouched, and it can still do it itself.
        assert!(progression_of(&w, &b).is_empty());
        assert_eq!(go(&mut w, &b, 200.0, 0.0, &mut now), MoveOutcome::Applied);
        assert_eq!(progression_of(&w, &b), STORY_ADVANCED);
        // ...which puts it back in A's instance, where each sees the other again.
        assert_eq!(w.instance_of(a.account), w.instance_of(b.account));
        assert_eq!(w.instances.len(), 1, "the emptied instance is released");
        let (a_msgs, b_msgs) = (a.drain(), b.drain());
        assert!(join_of(&a_msgs, b.entity), "{a_msgs:?}");
        assert_eq!(snapshots(&b_msgs).last().map(|s| s.0.clone()), Some(vec![a.entity, b.entity]));
    }

    #[test]
    fn a_chord_that_passes_through_the_region_advances_even_when_it_ends_beyond() {
        let t0 = Instant::now();
        let (mut w, _saved) = story_world();
        let a = seat(&mut w, "A", &[], t0);
        let mut now = t0;
        assert_eq!(go(&mut w, &a, 100.0, 100.0, &mut now), MoveOutcome::Applied);
        assert_eq!(go(&mut w, &a, 200.0, 100.0, &mut now), MoveOutcome::Applied);
        assert!(progression_of(&w, &a).is_empty(), "(200, 100) is above the region");
        // 200 units in one move, from above the region to below it: the chord goes through.
        assert_eq!(go(&mut w, &a, 200.0, -100.0, &mut now), MoveOutcome::Applied);
        assert_eq!(progression_of(&w, &a), STORY_ADVANCED);
    }

    #[test]
    fn a_party_is_placed_by_its_leader_and_the_members_keep_their_own_progression() {
        let t0 = Instant::now();
        let (mut w, mut saved) = story_world();
        let mut a = seat(&mut w, "A", STORY_ADVANCED, t0);
        let mut b = seat(&mut w, "B", &[], t0);
        // A joined already advanced: it was told where its story stands, and the two are apart.
        let a_msgs = a.drain();
        assert_eq!(story_updates(&a_msgs), [(STORY_ADVANCED.to_vec(), false)], "an account that advanced before is told at join");
        assert_eq!(placements(&a_msgs)[0].key.phase, advanced_phase());
        assert!(story_updates(&b.drain()).is_empty());
        assert_ne!(w.instance_of(a.account), w.instance_of(b.account));
        let mut now = t0;

        // They cannot see each other, but A may still invite the entity it knows (the protocol allows it).
        assert_eq!(w.party_invite(a.entity, b.entity, now), PartyOutcome::Done);
        let invite: InviteId = w.parties.invites().next().expect("an invite").id;
        a.drain();
        b.drain();
        assert_eq!(w.party_accept(b.entity, invite, now), PartyOutcome::Done);

        // B is now placed by A, the leader: A's instance. It keeps its own progression.
        assert_eq!(w.instance_of(a.account), w.instance_of(b.account));
        assert!(progression_of(&w, &b).is_empty());
        let b_msgs = b.drain();
        let last = placements(&b_msgs).pop().expect("a placement");
        assert_eq!((last.key.phase.clone(), last.phase_owner, last.story_advance_allowed), (advanced_phase(), a.account, false));
        assert!(last.party.is_some());
        assert_eq!(snapshots(&b_msgs).last().map(|s| s.0.clone()), Some(vec![a.entity, b.entity]), "B sees A again");
        assert!(join_of(&a.drain(), b.entity));

        // In A's instance B's own story is not advanced: its phase is not the one that chose the instance.
        assert_eq!(go(&mut w, &b, 100.0, 0.0, &mut now), MoveOutcome::Applied);
        assert_eq!(go(&mut w, &b, 200.0, 0.0, &mut now), MoveOutcome::Applied);
        assert!(progression_of(&w, &b).is_empty(), "story_advance_allowed was false");
        assert!(saved_stories(&mut saved).is_empty());

        // Leaving the party puts B back in the instance of its own phase (A hears it leave)...
        a.drain();
        b.drain();
        assert_eq!(w.party_leave(b.entity), PartyOutcome::Done);
        assert_ne!(w.instance_of(a.account), w.instance_of(b.account));
        assert!(leave_of(&a.drain(), b.entity));
        let back = b.drain();
        assert_eq!(placements(&back).last().map(|p| (p.key.phase.clone(), p.party, p.story_advance_allowed)), Some((PhaseKey::default(), None, true)));
        assert_eq!(snapshots(&back).last().map(|s| s.0.clone()), Some(vec![b.entity]));
        // ...where it may advance its story itself.
        assert_eq!(go(&mut w, &b, 190.0, 0.0, &mut now), MoveOutcome::Applied);
        assert_eq!(progression_of(&w, &b), STORY_ADVANCED);
    }

    #[test]
    fn a_leader_that_advances_takes_the_members_along_and_they_keep_their_own_progression() {
        let t0 = Instant::now();
        let (mut w, mut saved) = story_world();
        let mut a = seat(&mut w, "A", &[], t0);
        let mut b = seat(&mut w, "B", &[], t0);
        let mut now = t0;
        assert_eq!(w.party_invite(a.entity, b.entity, now), PartyOutcome::Done);
        let invite = w.parties.invites().next().expect("an invite").id;
        assert_eq!(w.party_accept(b.entity, invite, now), PartyOutcome::Done);
        a.drain();
        b.drain();
        assert_eq!(w.instance_of(a.account), w.instance_of(b.account));

        // The leader walks into the region: its story advances, and so does the party's instance.
        assert_eq!(go(&mut w, &a, 100.0, 0.0, &mut now), MoveOutcome::Applied);
        assert_eq!(go(&mut w, &a, 200.0, 0.0, &mut now), MoveOutcome::Applied);
        assert_eq!(saved_stories(&mut saved), [(a.account, STORY_ADVANCED.to_vec())], "only the leader's story was saved");
        assert!(progression_of(&w, &b).is_empty(), "B's story is its own and did not move");
        assert_eq!(w.instance_of(a.account), w.instance_of(b.account), "the party stays together");
        let b_msgs = b.drain();
        assert!(story_updates(&b_msgs).is_empty(), "B is told nothing about A's story");
        let p = placements(&b_msgs).pop().expect("B's placement changed with the leader's phase");
        assert_eq!((p.key.phase.clone(), p.phase_owner, p.story_advance_allowed), (advanced_phase(), a.account, false));
        assert_eq!(snapshots(&b_msgs).last().map(|s| s.0.clone()), Some(vec![a.entity, b.entity]));
        // Promote B: the instance follows B's phase (the empty one) and A is re-placed.
        a.drain();
        assert_eq!(w.party_promote(a.entity, b.entity), PartyOutcome::Done);
        assert_eq!(w.instance_of(a.account), w.instance_of(b.account));
        let p = placements(&a.drain()).pop().expect("A is re-placed");
        assert_eq!((p.key.phase.clone(), p.phase_owner, p.story_advance_allowed), (PhaseKey::default(), b.account, false));
    }

    #[test]
    fn an_account_arriving_in_another_area_is_placed_by_its_phase_there_and_the_cast_follows_the_phase() {
        let t0 = Instant::now();
        let (mut w, _saved) = story_world();
        let mut a = seat(&mut w, "A", STORY_ADVANCED, t0);
        let mut b = seat(&mut w, "B", &[], t0);
        a.drain();
        b.drain();
        let mut now = t0;
        // Both walk through the hall's exit to the yard, above the story region (y 150, region y -50..50).
        for s in [&a, &b] {
            assert_eq!(go(&mut w, s, 100.0, 150.0, &mut now), MoveOutcome::Applied);
            assert_eq!(go(&mut w, s, 350.0, 150.0, &mut now), MoveOutcome::Applied);
        }
        assert!(progression_of(&w, &b).is_empty(), "B kept clear of the region");
        for s in [&mut a, &mut b] {
            let msgs = s.drain();
            let offer = msgs.iter().find(|m| matches!(m, ServerMsg::AreaChange { .. })).cloned().expect("an offer");
            let ServerMsg::AreaChange { area, fingerprint, provenance, .. } = offer else { unreachable!() };
            assert_eq!(w.join_area(s.entity, &area, &fingerprint, &provenance, now), crate::world::TravelReply::Done);
        }
        // In the yard the two are in different instances (the yard reads the story too): the guard is
        // in the advanced phase's cast only, and neither sees the other.
        assert_ne!(w.instance_of(a.account), w.instance_of(b.account));
        let (a_msgs, b_msgs) = (a.drain(), b.drain());
        assert_eq!(snapshots(&a_msgs), [(vec![a.entity], 1)], "A's yard has the guard");
        assert_eq!(snapshots(&b_msgs), [(vec![b.entity], 0)], "B's yard has none");
        let (pa, pb) = (placements(&a_msgs).pop().unwrap(), placements(&b_msgs).pop().unwrap());
        assert_eq!((pa.key.area.key.as_str(), pa.key.phase), (YARD, advanced_phase()));
        assert_eq!((pb.key.area.key.as_str(), pb.key.phase), (YARD, PhaseKey::default()));
    }

    #[test]
    fn a_progression_the_area_cannot_read_is_its_own_phase_and_is_never_advanced_or_saved_over() {
        let t0 = Instant::now();
        let (mut w, mut saved) = story_world();
        let mut odd = seat(&mut w, "Odd", &[7, 7], t0);
        let mut plain = seat(&mut w, "Plain", &[], t0);
        // An opaque blob keeps the old identity: one phase per distinct blob, so a lone instance.
        let msgs = odd.drain();
        assert_eq!(placements(&msgs)[0].key.phase, PhaseKey(vec![7, 7]));
        assert_ne!(w.instance_of(odd.account), w.instance_of(plain.account));
        plain.drain();
        let mut now = t0;
        assert_eq!(go(&mut w, &odd, 100.0, 0.0, &mut now), MoveOutcome::Applied);
        assert_eq!(go(&mut w, &odd, 200.0, 0.0, &mut now), MoveOutcome::Applied);
        assert_eq!(progression_of(&w, &odd), vec![7, 7], "never advanced");
        assert!(saved_stories(&mut saved).is_empty(), "and never written over");
        assert!(!log_text(&mut w).contains("story advanced"));
    }

    #[test]
    fn leaving_the_world_leaves_the_instance_and_the_last_one_out_releases_it() {
        let t0 = Instant::now();
        let (mut w, _saved) = story_world();
        let a = seat(&mut w, "A", STORY_ADVANCED, t0);
        let mut b = seat(&mut w, "B", &[], t0);
        let c = seat(&mut w, "C", &[], t0);
        b.drain();
        assert_eq!(w.instances.len(), 2);
        assert!(w.leave(a.entity));
        assert_eq!(w.instances.len(), 1, "A was alone in its instance");
        assert!(b.drain().is_empty(), "nobody in B's instance heard of A");
        assert!(w.leave(c.entity));
        assert!(leave_of(&b.drain(), c.entity));
        assert!(w.leave(b.entity));
        assert_eq!(w.instances.len(), 0);
    }
}
