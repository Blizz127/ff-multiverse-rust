//! Shared test helpers: account constants, a client-side replica that is driven *only* by
//! events, and per-account view snapshots used to check that event audiences are exact.
#![allow(dead_code)]

use fmv_core::AccountId;
use fmv_party::{InviteId, Party, PartyEvent, PartyId, PartyManager, Tick};
use std::collections::{BTreeMap, BTreeSet};

pub const A: AccountId = AccountId(1);
pub const B: AccountId = AccountId(2);
pub const C: AccountId = AccountId(3);
pub const D: AccountId = AccountId(4);
pub const E: AccountId = AccountId(5);
pub const F: AccountId = AccountId(6);

pub fn pid(n: u64) -> PartyId {
    PartyId(n)
}

pub fn iid(n: u64) -> InviteId {
    InviteId(n)
}

/// What one client could know: its party and the invites to and from it.
pub type View = (Option<Party>, Vec<InviteId>, Vec<InviteId>);

pub fn view(m: &PartyManager, who: AccountId) -> View {
    (
        m.party_of(who).cloned(),
        m.invites_to(who).map(|i| i.id).collect(),
        m.invites_from(who).map(|i| i.id).collect(),
    )
}

/// A mirror of the party state built exclusively by applying [`PartyEvent`]s in order.
/// Panics (test failure) if an event contradicts the mirror, which would mean the events are
/// not a faithful, replayable description of the manager's changes.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Replica {
    pub parties: BTreeMap<PartyId, Party>,
    /// invite -> (from, to, expires_at)
    pub invites: BTreeMap<InviteId, (AccountId, AccountId, Tick)>,
}

impl Replica {
    pub fn apply_all(&mut self, events: &[PartyEvent]) {
        for event in events {
            self.apply(event);
        }
    }

    fn take_invite(&mut self, invite: InviteId, from: AccountId, to: AccountId) {
        let (f, t, _) = self
            .invites
            .remove(&invite)
            .unwrap_or_else(|| panic!("event refers to unknown invite {invite:?}"));
        assert_eq!(
            (f, t),
            (from, to),
            "event names the wrong parties for {invite:?}"
        );
    }

    pub fn apply(&mut self, event: &PartyEvent) {
        match event {
            PartyEvent::InviteSent {
                invite,
                from,
                to,
                expires_at,
            } => {
                let prev = self.invites.insert(*invite, (*from, *to, *expires_at));
                assert!(prev.is_none(), "invite id announced twice");
            }
            PartyEvent::InviteDeclined { invite, from, to }
            | PartyEvent::InviteExpired { invite, from, to }
            | PartyEvent::InviteCancelled {
                invite, from, to, ..
            } => {
                self.take_invite(*invite, *from, *to);
            }
            PartyEvent::PartyFormed {
                party,
                invite,
                leader,
                members,
            } => {
                assert_eq!(members.len(), 2);
                assert_eq!(members[0], *leader);
                self.take_invite(*invite, *leader, members[1]);
                let prev = self.parties.insert(
                    *party,
                    Party {
                        id: *party,
                        leader: *leader,
                        members: members.clone(),
                    },
                );
                assert!(prev.is_none(), "party id announced twice");
            }
            PartyEvent::MemberJoined {
                party,
                invite,
                account,
                leader,
                members,
            } => {
                let p = self
                    .parties
                    .get_mut(party)
                    .expect("join into unknown party");
                self.invites
                    .remove(invite)
                    .map(|(f, t, _)| assert_eq!((f, t), (*leader, *account)))
                    .expect("join consumed an unknown invite");
                p.members.push(*account);
                assert_eq!(
                    &p.members, members,
                    "MemberJoined roster disagrees with the replica"
                );
                assert_eq!(p.leader, *leader);
            }
            PartyEvent::MemberLeft {
                party,
                account,
                members,
                ..
            } => {
                let p = self
                    .parties
                    .get_mut(party)
                    .expect("leave from unknown party");
                assert!(p.members.contains(account));
                p.members.retain(|m| m != account);
                assert_eq!(
                    &p.members, members,
                    "MemberLeft roster disagrees with the replica"
                );
            }
            PartyEvent::LeaderChanged {
                party,
                old_leader,
                new_leader,
                members,
                ..
            } => {
                let p = self
                    .parties
                    .get_mut(party)
                    .expect("leader change in unknown party");
                assert_eq!(p.leader, *old_leader);
                assert_eq!(
                    &p.members, members,
                    "LeaderChanged roster disagrees with the replica"
                );
                assert!(p.members.contains(new_leader));
                p.leader = *new_leader;
            }
            PartyEvent::Disbanded { party, members, .. } => {
                let p = self
                    .parties
                    .remove(party)
                    .expect("disband of unknown party");
                assert_eq!(
                    &p.members, members,
                    "Disbanded roster disagrees with the replica"
                );
            }
        }
        // Every prefix of an event stream is a valid state: a party always contains its leader.
        for p in self.parties.values() {
            assert!(
                p.members.contains(&p.leader),
                "intermediate state lost its leader: {p:?}"
            );
            assert!(!p.members.is_empty());
        }
    }

    /// The replica must equal the manager's authoritative state.
    pub fn assert_mirrors(&self, m: &PartyManager) {
        let parties: BTreeMap<PartyId, Party> = m.parties().map(|p| (p.id, p.clone())).collect();
        assert_eq!(
            self.parties, parties,
            "replica parties diverged from the manager"
        );
        let invites: BTreeMap<InviteId, (AccountId, AccountId, Tick)> = m
            .invites()
            .map(|i| (i.id, (i.from, i.to, i.expires_at)))
            .collect();
        assert_eq!(
            self.invites, invites,
            "replica invites diverged from the manager"
        );
    }
}

/// Accounts that appear in the audience of at least one event.
pub fn told(events: &[PartyEvent]) -> BTreeSet<AccountId> {
    events.iter().flat_map(|e| e.audience()).collect()
}

/// Assert that exactly the accounts whose view changed were told, and nobody else.
pub fn assert_audience_exact(
    universe: &[AccountId],
    before: &BTreeMap<AccountId, View>,
    m: &PartyManager,
    events: &[PartyEvent],
) {
    let changed: BTreeSet<AccountId> = universe
        .iter()
        .copied()
        .filter(|a| before.get(a) != Some(&view(m, *a)))
        .collect();
    assert_eq!(
        told(events),
        changed,
        "audience mismatch for events {events:?}: told vs accounts whose state changed"
    );
}

pub fn snapshot_views(m: &PartyManager, universe: &[AccountId]) -> BTreeMap<AccountId, View> {
    universe.iter().map(|a| (*a, view(m, *a))).collect()
}

fn kind(event: &PartyEvent) -> String {
    match event {
        PartyEvent::MemberLeft { reason, .. } => format!("MemberLeft:{reason:?}"),
        PartyEvent::LeaderChanged { reason, .. } => format!("LeaderChanged:{reason:?}"),
        PartyEvent::InviteCancelled { reason, .. } => format!("InviteCancelled:{reason:?}"),
        other => format!("{other:?}")
            .split([' ', '{'])
            .next()
            .unwrap_or("?")
            .to_string(),
    }
}

/// Wraps a [`PartyManager`] and, around *every* operation, checks that:
/// * a refused operation changed nothing (atomicity),
/// * `validate()` passes,
/// * a replica driven only by the returned events equals the manager (events are exact and
///   replayable, every prefix valid),
/// * exactly the accounts whose state changed were told (audience is exact).
pub struct Harness {
    pub m: PartyManager,
    pub replica: Replica,
    /// Every account that may appear in any operation, including unknown ones.
    pub universe: Vec<AccountId>,
    /// Event kinds produced so far (coverage evidence).
    pub events_seen: BTreeSet<String>,
    /// Error variants returned so far (coverage evidence).
    pub errors_seen: BTreeSet<String>,
    /// Every event emitted so far, in order (for determinism comparisons).
    pub log: Vec<PartyEvent>,
}

impl Harness {
    pub fn new(m: PartyManager, universe: Vec<AccountId>) -> Self {
        Self {
            m,
            replica: Replica::default(),
            universe,
            events_seen: BTreeSet::new(),
            errors_seen: BTreeSet::new(),
            log: Vec::new(),
        }
    }

    fn run<T>(
        &mut self,
        op: impl FnOnce(&mut PartyManager) -> Result<(T, Vec<PartyEvent>), fmv_party::PartyError>,
    ) -> Result<(T, Vec<PartyEvent>), fmv_party::PartyError> {
        let before_state = self.m.clone();
        let before_views = snapshot_views(&self.m, &self.universe);
        let result = op(&mut self.m);
        self.m.validate().expect("manager invariants broken");
        match &result {
            Err(e) => {
                assert_eq!(
                    self.m, before_state,
                    "refused operation {e:?} changed state"
                );
                let name = format!("{e:?}");
                self.errors_seen.insert(
                    name.split([' ', '{', '('])
                        .next()
                        .unwrap_or("?")
                        .to_string(),
                );
            }
            Ok((_, events)) => {
                self.replica.apply_all(events);
                self.replica.assert_mirrors(&self.m);
                assert_audience_exact(&self.universe, &before_views, &self.m, events);
                for e in events {
                    self.events_seen.insert(kind(e));
                }
                self.log.extend(events.iter().cloned());
            }
        }
        result
    }

    pub fn invite(
        &mut self,
        from: AccountId,
        to: AccountId,
        now: Tick,
    ) -> Result<(InviteId, Vec<PartyEvent>), fmv_party::PartyError> {
        self.run(|m| m.invite(from, to, now))
    }

    pub fn accept(
        &mut self,
        invite: InviteId,
        by: AccountId,
        now: Tick,
    ) -> Result<Vec<PartyEvent>, fmv_party::PartyError> {
        self.run(|m| m.accept(invite, by, now).map(|e| ((), e)))
            .map(|(_, e)| e)
    }

    pub fn decline(
        &mut self,
        invite: InviteId,
        by: AccountId,
    ) -> Result<Vec<PartyEvent>, fmv_party::PartyError> {
        self.run(|m| m.decline(invite, by).map(|e| ((), e)))
            .map(|(_, e)| e)
    }

    pub fn leave(&mut self, member: AccountId) -> Result<Vec<PartyEvent>, fmv_party::PartyError> {
        self.run(|m| m.leave(member).map(|e| ((), e)))
            .map(|(_, e)| e)
    }

    pub fn kick(
        &mut self,
        leader: AccountId,
        member: AccountId,
    ) -> Result<Vec<PartyEvent>, fmv_party::PartyError> {
        self.run(|m| m.kick(leader, member).map(|e| ((), e)))
            .map(|(_, e)| e)
    }

    pub fn promote(
        &mut self,
        leader: AccountId,
        member: AccountId,
    ) -> Result<Vec<PartyEvent>, fmv_party::PartyError> {
        self.run(|m| m.promote(leader, member).map(|e| ((), e)))
            .map(|(_, e)| e)
    }

    pub fn expire(&mut self, now: Tick) -> Result<Vec<PartyEvent>, fmv_party::PartyError> {
        self.run(|m| m.expire(now).map(|e| ((), e))).map(|(_, e)| e)
    }
}

/// Tiny deterministic generator so the randomized test needs no external crates.
pub struct Lcg(pub u64);

impl Lcg {
    pub fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}
