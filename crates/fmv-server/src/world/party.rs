//! Party requests, as `World` methods.
//!
//! Each method acts for the account of the REQUESTING session (looked up from its entity), never
//! for an account named in a message. A request either succeeds (every state change has already
//! been routed to its audience) or is refused with the reason for the requester only
//! ([`PartyOutcome::Refused`]); `Gone` means the requester is no longer in the world. The rules
//! themselves are `fmv_party::PartyManager`'s; this file only adds what it leaves to the caller:
//! entity-to-account mapping, presence, and a rate limit on invites.

use std::sync::Arc;
use std::time::{Duration, Instant};

use fmv_core::{AccountId, EntityId};
use fmv_party::{Invite, InviteId, PartyEvent};
use fmv_protocol::{PartyFailure, ServerMsg};

use super::{is_full, World};

/// A player may create at most [`MAX_INVITES_PER_WINDOW`] invites per this window. Pending
/// invites are bounded only by request rate times the invite lifetime, so the server limits the
/// requests before they reach the party manager.
pub const INVITE_WINDOW: Duration = Duration::from_secs(10);
pub const MAX_INVITES_PER_WINDOW: usize = 5;

/// What became of one party request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PartyOutcome {
    /// Applied; the resulting events were already queued for their audience.
    Done,
    /// Refused; tell the requester (`ServerMsg::PartyRejected`) and nobody else.
    Refused(PartyFailure),
    /// The requester is no longer in the world.
    Gone,
}

impl World {
    fn account_of(&self, entity: EntityId) -> Option<AccountId> {
        self.players.get(&entity).map(|p| p.account)
    }

    /// A membership or leadership change can re-phase every member, including someone other
    /// than the requester. Pending story and battle saves own their cohorts through acknowledgment.
    fn party_change_held(&self, account: AccountId) -> bool {
        let held = |account| self.continuation_account_held(account)
            || self.ordinary_story_account_held(account) || self.ordinary_battle_account_held(account);
        held(account)
            || self.parties.party_of(account).is_some_and(|party|
                party.members.iter().any(|member| held(*member)))
    }

    fn invite_change_held(&self, account: AccountId, invite: InviteId) -> bool {
        self.party_change_held(account)
            || self.parties.get_invite(invite).filter(|invite| invite.to == account)
                .is_some_and(|invite| self.party_change_held(invite.from))
    }

    /// Route `events` to their audiences and drop whoever's queue failed on the way.
    fn deliver(&mut self, events: Vec<PartyEvent>) -> PartyOutcome {
        let mut failed = Vec::new();
        self.route_party_events(events, &[], &mut failed);
        self.drop_failed(failed);
        PartyOutcome::Done
    }

    fn finish(&mut self, result: Result<Vec<PartyEvent>, fmv_party::PartyError>) -> PartyOutcome {
        match result {
            Ok(events) => self.deliver(events),
            Err(e) => PartyOutcome::Refused(PartyFailure::Rules(e)),
        }
    }

    /// `actor` invites the player with entity `target` (into its party, or to form one).
    pub fn party_invite(&mut self, actor: EntityId, target: EntityId, now: Instant) -> PartyOutcome {
        let Some(account) = self.account_of(actor) else { return PartyOutcome::Gone; };
        let Some(target_account) = self.account_of(target) else {
            return PartyOutcome::Refused(PartyFailure::UnknownPlayer);
        };
        if self.party_change_held(account) || self.party_change_held(target_account) {
            return PartyOutcome::Refused(PartyFailure::Busy);
        }
        let Some(player) = self.players.get_mut(&actor) else {
            return PartyOutcome::Gone;
        };
        while player.invites.front().is_some_and(|t| now.saturating_duration_since(*t) >= INVITE_WINDOW) {
            player.invites.pop_front();
        }
        if player.invites.len() >= MAX_INVITES_PER_WINDOW {
            return PartyOutcome::Refused(PartyFailure::RateLimited);
        }
        let clock = self.party_clock(now);
        match self.parties.invite(account, target_account, clock) {
            Ok((_invite, events)) => {
                if let Some(player) = self.players.get_mut(&actor) {
                    player.invites.push_back(now);
                }
                self.deliver(events)
            }
            Err(e) => PartyOutcome::Refused(PartyFailure::Rules(e)),
        }
    }

    /// `actor` accepts an invite addressed to its account.
    pub fn party_accept(&mut self, actor: EntityId, invite: InviteId, now: Instant) -> PartyOutcome {
        let Some(account) = self.account_of(actor) else {
            return PartyOutcome::Gone;
        };
        if self.invite_change_held(account, invite) { return PartyOutcome::Refused(PartyFailure::Busy); }
        let clock = self.party_clock(now);
        let result = self.parties.accept(invite, account, clock);
        self.finish(result)
    }

    /// `actor` declines an invite addressed to its account.
    pub fn party_decline(&mut self, actor: EntityId, invite: InviteId) -> PartyOutcome {
        let Some(account) = self.account_of(actor) else {
            return PartyOutcome::Gone;
        };
        if self.invite_change_held(account, invite) { return PartyOutcome::Refused(PartyFailure::Busy); }
        let result = self.parties.decline(invite, account);
        self.finish(result)
    }

    /// `actor` leaves its party.
    pub fn party_leave(&mut self, actor: EntityId) -> PartyOutcome {
        let Some(account) = self.account_of(actor) else {
            return PartyOutcome::Gone;
        };
        if self.party_change_held(account) { return PartyOutcome::Refused(PartyFailure::Busy); }
        let result = self.parties.leave(account);
        self.finish(result)
    }

    /// The leader `actor` removes the member with entity `target`.
    pub fn party_kick(&mut self, actor: EntityId, target: EntityId) -> PartyOutcome {
        let Some(account) = self.account_of(actor) else {
            return PartyOutcome::Gone;
        };
        let Some(target_account) = self.account_of(target) else {
            return PartyOutcome::Refused(PartyFailure::UnknownPlayer);
        };
        if self.party_change_held(account) || self.party_change_held(target_account) {
            return PartyOutcome::Refused(PartyFailure::Busy);
        }
        let result = self.parties.kick(account, target_account);
        self.finish(result)
    }

    /// The leader `actor` hands leadership to the member with entity `target`.
    pub fn party_promote(&mut self, actor: EntityId, target: EntityId) -> PartyOutcome {
        let Some(account) = self.account_of(actor) else {
            return PartyOutcome::Gone;
        };
        let Some(target_account) = self.account_of(target) else {
            return PartyOutcome::Refused(PartyFailure::UnknownPlayer);
        };
        if self.party_change_held(account) || self.party_change_held(target_account) {
            return PartyOutcome::Refused(PartyFailure::Busy);
        }
        let result = self.parties.promote(account, target_account);
        self.finish(result)
    }

    /// Send `actor` its whole party situation: its party and every live invite to or from it.
    pub fn party_sync(&mut self, actor: EntityId, now: Instant) -> PartyOutcome {
        let Some(account) = self.account_of(actor) else {
            return PartyOutcome::Gone;
        };
        let clock = self.party_clock(now);
        let party = self.parties.party_of(account).cloned();
        let invites: Vec<Invite> = self
            .parties
            .invites_to(account)
            .chain(self.parties.invites_from(account))
            .filter(|i| !i.is_expired(clock))
            .copied()
            .collect();
        let mut accounts = vec![account];
        if let Some(party) = &party {
            accounts.extend(party.members.iter().copied());
        }
        for invite in &invites {
            accounts.extend([invite.from, invite.to]);
        }
        accounts.sort();
        accounts.dedup();
        let names = self.names_for(&accounts, &[]);
        let Some(player) = self.players.get_mut(&actor) else {
            return PartyOutcome::Gone;
        };
        let msg = ServerMsg::PartyState { seq: player.party_seq, party, invites, names };
        if let Err(e) = player.tx.try_send(Arc::new(msg)) {
            let failed = vec![(actor, is_full(&e))];
            self.drop_failed(failed);
        }
        PartyOutcome::Done
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::{NewPlayer, Outbound, SceneBoundary, SceneBoundaryKind, SceneHost, SceneInfo, SceneResult, SceneSettings, TriggerInfo};
    use fmv_adapter_api::GameAdapter;
    use fmv_core::{AvatarAppearance, Pos};
    use fmv_fixture::{FixtureAdapter, FIXTURE_AREA_KEY};
    use tokio::sync::{mpsc, watch};

    struct HeldScene;
    impl SceneHost for HeldScene {
        fn scenes(&self) -> Vec<SceneInfo> {
            vec![SceneInfo { id: "fixture/held".into(), field: FIXTURE_AREA_KEY.into(), arrival: Pos::default(),
                arrival_facing: 0.0, arrival_hint: None }]
        }
        fn offered(&self, _: &str, _: &[u8]) -> bool { true }
        fn automatic(&self, _: &str) -> bool { true }
        fn continuation(&self, _: &str) -> bool { true }
        fn trigger(&self, _: &str, _: &[u8]) -> Option<TriggerInfo> { None }
        fn preview_progress(&self, _: &str) -> Option<Vec<u8>> { None }
        fn finish(&self, _: &str, _: &[u8], _: u32, _: &[u8]) -> Option<SceneResult> { None }
        fn evaluate_continuation(&self, _: &str, progress: &[u8], _: u32, gil: u32,
            _: Option<&fmv_protocol::SceneReplay>) -> Option<SceneBoundary> {
            Some(SceneBoundary { source: [7; 32], progress: progress.to_vec(), gil,
                at: SceneBoundaryKind::Window { step: 0, identity: vec![1], choices: None } })
        }
    }

    fn seat(world: &mut World, now: Instant) -> (EntityId, AccountId, mpsc::Receiver<Outbound>) {
        let (session, entity) = world.allocate_ids();
        let account = AccountId(entity.0);
        let (tx, rx) = mpsc::channel(128);
        world.join(NewPlayer { session, entity, account,
            avatar: AvatarAppearance { display_name: format!("player{}", entity.0), tint: [0; 3], appearance_ref: None },
            progression: Vec::new(), progress: Default::default(), scene_state: None, scene_group: Vec::new(),
            tx, close: Arc::new(watch::channel(false).0) }, now).unwrap();
        (entity, account, rx)
    }

    fn invite(world: &mut World, from: EntityId, to: EntityId, account: AccountId, now: Instant) -> InviteId {
        assert_eq!(world.party_invite(from, to, now), PartyOutcome::Done);
        let inviter = world.account_of(from).unwrap();
        world.parties.invites_to(account).find(|invite| invite.from == inviter).unwrap().id
    }

    #[test]
    fn party_mutations_cannot_rephase_a_held_member_even_when_the_requester_is_not_held() {
        let now = Instant::now();
        let mut world = World::new(FixtureAdapter::new().load_area(FIXTURE_AREA_KEY).unwrap()).unwrap();
        let (saver, mut saves) = crate::saver::SaverHandle::channel();
        world.set_progress_saver(saver);
        let (a, aa, mut arx) = seat(&mut world, now);
        let (b, ba, mut brx) = seat(&mut world, now);
        let (c, ca, mut crx) = seat(&mut world, now);
        let (d, _, mut drx) = seat(&mut world, now);
        let into_party = invite(&mut world, a, b, ba, now);
        assert_eq!(world.party_accept(b, into_party, now), PartyOutcome::Done);
        let pending = invite(&mut world, a, c, ca, now);
        world.configure_scenes(Some(SceneSettings { host: Arc::new(HeldScene), debug_preview: false, debug_new_story: None }));
        world.automatic_scene_entered(b, now);
        assert!(world.continuation_held(b) && !world.continuation_held(a));
        let before = world.parties.party_of(aa).cloned();
        let held = world.players.get(&b).unwrap();
        let held_before = (held.scene, held.placement.clone(), held.snapshot.pos, held.progression.clone());
        for rx in [&mut arx, &mut brx, &mut crx, &mut drx] { while rx.try_recv().is_ok() {} }
        let busy = PartyOutcome::Refused(PartyFailure::Busy);
        assert_eq!(world.party_leave(a), busy);
        assert_eq!(world.party_leave(b), busy);
        assert_eq!(world.party_kick(a, b), busy);
        assert_eq!(world.party_promote(a, b), busy);
        assert_eq!(world.party_invite(a, d, now), busy);
        assert_eq!(world.party_invite(d, b, now), busy);
        assert_eq!(world.party_accept(c, pending, now), busy);
        assert_eq!(world.party_decline(c, pending), busy);
        assert_eq!(world.parties.party_of(aa), before.as_ref());
        assert!(world.parties.get_invite(pending).is_some());
        let held = world.players.get(&b).unwrap();
        assert_eq!((held.scene, held.placement.clone(), held.snapshot.pos, held.progression.clone()), held_before);
        assert!(saves.try_recv().is_err(), "no party refusal may enqueue an ordinary save");
        for rx in [&mut arx, &mut brx, &mut crx, &mut drx] { assert!(rx.try_recv().is_err()); }
        assert_eq!(world.party_sync(b, now), PartyOutcome::Done, "read-only sync stays available");
        assert!(matches!(&*brx.try_recv().unwrap(), ServerMsg::PartyState { .. }));
        // An unrelated party still forms normally while this continuation holds its own cohort.
        let independent = invite(&mut world, d, c, ca, now);
        assert_eq!(world.party_accept(c, independent, now), PartyOutcome::Done);
        assert_eq!(world.parties.party_of(aa), before.as_ref());
        // Disconnect cleanup may dissolve the party, but cannot replace a held player's
        // field snapshot or phase while the original scene is still running.
        while brx.try_recv().is_ok() {}
        assert!(world.leave(a));
        let held = world.players.get(&b).unwrap();
        assert_eq!((held.scene, held.placement.clone(), held.snapshot.pos, held.progression.clone()), held_before);
        while let Ok(message) = brx.try_recv() {
            assert!(!matches!(&*message, ServerMsg::AreaSnapshot { .. } | ServerMsg::InstancePlacement { .. }
                | ServerMsg::SceneEnded { .. }), "disconnect changed held presentation: {message:?}");
        }
        // An acknowledged intermediate story write updates its mirror, but also defers the
        // placement and NPC snapshot until the continuation releases it.
        world.apply_saved_story_step(b, u32::MAX, fmv_adapter_api::StoryStep {
            progress: vec![3], summary: "durable intermediate state".into(), countdown: None,
        });
        assert_eq!(world.players.get(&b).unwrap().placement, held_before.1);
        while let Ok(message) = brx.try_recv() {
            assert!(!matches!(&*message, ServerMsg::AreaSnapshot { .. } | ServerMsg::InstancePlacement { .. }),
                "intermediate save changed held presentation: {message:?}");
        }
        assert!(saves.try_recv().is_err());
    }

    #[test]
    fn a_held_invitee_cannot_accept_or_decline_a_preexisting_invite() {
        let now = Instant::now();
        let mut world = World::new(FixtureAdapter::new().load_area(FIXTURE_AREA_KEY).unwrap()).unwrap();
        let (a, aa, _arx) = seat(&mut world, now);
        let (b, _, _brx) = seat(&mut world, now);
        let pending = invite(&mut world, b, a, aa, now);
        world.configure_scenes(Some(SceneSettings { host: Arc::new(HeldScene), debug_preview: false, debug_new_story: None }));
        world.automatic_scene_entered(a, now);
        assert!(world.continuation_held(a));
        assert_eq!(world.party_accept(a, pending, now), PartyOutcome::Refused(PartyFailure::Busy));
        assert_eq!(world.party_decline(a, pending), PartyOutcome::Refused(PartyFailure::Busy));
        assert!(world.parties.party_of(aa).is_none());
        assert!(world.parties.get_invite(pending).is_some());
    }
}
