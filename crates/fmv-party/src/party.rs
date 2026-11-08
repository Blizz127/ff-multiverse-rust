//! Party membership, invites and leadership.
//!
//! [`PartyManager`] is a pure state machine: no I/O, no clocks, no randomness. Every operation
//! either returns `Err` and leaves the state exactly as it was, or applies fully and returns the
//! [`PartyEvent`]s describing what happened (see [`crate::event`] for the event contract).
//!
//! # Rules
//!
//! * A party has one leader and at most [`PartyConfig::max_size`] members. Members are kept in
//!   join order; the first entry is the longest-standing member. A party of one is legal: a
//!   party only disbands when its last member leaves.
//! * Only the leader (or a solo player) may invite, and only players that are in no party.
//! * An invite does not name a party. When it is accepted it resolves against the inviter's
//!   *current* standing: a solo inviter forms a new party (`PartyFormed`), a leader inviter's
//!   party gains the member (`MemberJoined`). Two invites from one solo player can therefore
//!   both be accepted: the first forms the party, the second joins it.
//! * Invites that can no longer be honoured are cancelled eagerly with
//!   [`PartyEvent::InviteCancelled`], so every pending invite always has a solo-or-leader
//!   inviter and a party-less invitee (modulo capacity and expiry).
//! * Time: an invite created at `now` is acceptable while `now' < now + ttl`. Only
//!   [`PartyManager::expire`] removes timed-out invites (so only it announces `InviteExpired`),
//!   except that a successful [`PartyManager::invite`] replaces a timed-out invite for the same
//!   pair and announces that expiry first. A timed-out invite that has not been reaped yet is
//!   refused by `accept` with [`PartyError::InviteExpired`].
//! * Leader leaves: the longest-standing remaining member becomes leader. Promotion does not
//!   change seniority, so a demoted leader keeps their place in the succession order.
//!
//! # What the caller still owns
//!
//! * Authentication: pass the account the *session* belongs to as the acting account, never an
//!   id taken from a message.
//! * Account existence and presence: account ids are opaque here, so the server must check that
//!   an invitee is a real, online account, and decide what a disconnect means (call
//!   [`PartyManager::leave`], or keep the member for a grace period).
//! * Time: call [`PartyManager::expire`] regularly and fan out the returned events.
//! * Rate limiting: pending invites are bounded only by request rate times the invite ttl, so
//!   limit invite requests per sender before they reach the manager.

use crate::config::PartyConfig;
use crate::error::PartyError;
use crate::event::{
    DisbandReason, InviteCancelReason, LeaderChangeReason, LeaveReason, PartyEvent,
};
use crate::ids::{InviteId, PartyId, Tick};
use fmv_core::AccountId;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;

/// One party. `members` is in join order, oldest first, and always contains `leader`.
///
/// Fields are public so replicas and protocol messages can carry the same shape; only a
/// [`PartyManager`] can change the authoritative copy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Party {
    pub id: PartyId,
    pub leader: AccountId,
    pub members: Vec<AccountId>,
}

impl Party {
    pub fn contains(&self, account: AccountId) -> bool {
        self.members.contains(&account)
    }

    pub fn is_leader(&self, account: AccountId) -> bool {
        self.leader == account
    }

    /// Number of members, leader included. Never zero for a live party.
    pub fn size(&self) -> usize {
        self.members.len()
    }
}

/// A pending invite.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invite {
    pub id: InviteId,
    pub from: AccountId,
    pub to: AccountId,
    pub created_at: Tick,
    /// First tick at which the invite is no longer acceptable.
    pub expires_at: Tick,
}

impl Invite {
    /// True once `now` has reached `expires_at`.
    pub fn is_expired(&self, now: Tick) -> bool {
        now >= self.expires_at
    }
}

/// Which party an accepted invite lands in.
enum Join {
    NewParty(PartyId),
    Existing(PartyId),
}

/// Authoritative party state for one game. See the [module docs](self) for the rules.
///
/// ```
/// use fmv_core::AccountId;
/// use fmv_party::{PartyConfig, PartyEvent, PartyManager};
///
/// let mut parties = PartyManager::new(PartyConfig::default());
/// let (alice, bob) = (AccountId(1), AccountId(2));
///
/// let (invite, events) = parties.invite(alice, bob, 0).unwrap();
/// assert!(matches!(events[0], PartyEvent::InviteSent { .. }));
///
/// let events = parties.accept(invite, bob, 5).unwrap();
/// assert!(matches!(events[0], PartyEvent::PartyFormed { .. }));
/// assert_eq!(parties.party_of(bob).unwrap().leader, alice);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartyManager {
    config: PartyConfig,
    /// Next id to hand out; ids are never reused.
    next_party: u64,
    next_invite: u64,
    parties: BTreeMap<PartyId, Party>,
    invites: BTreeMap<InviteId, Invite>,
    /// Derived index: account -> party. Rebuilt (and checked) on deserialization.
    member_party: BTreeMap<AccountId, PartyId>,
}

impl Default for PartyManager {
    fn default() -> Self {
        Self::new(PartyConfig::default())
    }
}

impl PartyManager {
    pub fn new(config: PartyConfig) -> Self {
        Self {
            config,
            next_party: 1,
            next_invite: 1,
            parties: BTreeMap::new(),
            invites: BTreeMap::new(),
            member_party: BTreeMap::new(),
        }
    }

    pub fn config(&self) -> &PartyConfig {
        &self.config
    }

    // ----- queries -------------------------------------------------------------------------

    pub fn party(&self, id: PartyId) -> Option<&Party> {
        self.parties.get(&id)
    }

    /// The party `account` belongs to, or `None` if they are solo.
    pub fn party_of(&self, account: AccountId) -> Option<&Party> {
        self.member_party
            .get(&account)
            .and_then(|id| self.parties.get(id))
    }

    pub fn party_id_of(&self, account: AccountId) -> Option<PartyId> {
        self.member_party.get(&account).copied()
    }

    /// True if `account` leads a party (a solo player leads nothing).
    pub fn is_leader(&self, account: AccountId) -> bool {
        self.party_of(account).is_some_and(|p| p.leader == account)
    }

    /// All parties in id order.
    pub fn parties(&self) -> impl Iterator<Item = &Party> + '_ {
        self.parties.values()
    }

    pub fn party_count(&self) -> usize {
        self.parties.len()
    }

    pub fn get_invite(&self, id: InviteId) -> Option<&Invite> {
        self.invites.get(&id)
    }

    /// All pending invites in id (creation) order, including timed-out ones not yet reaped.
    pub fn invites(&self) -> impl Iterator<Item = &Invite> + '_ {
        self.invites.values()
    }

    pub fn invite_count(&self) -> usize {
        self.invites.len()
    }

    /// Pending invites addressed to `account`, in id order.
    pub fn invites_to(&self, account: AccountId) -> impl Iterator<Item = &Invite> + '_ {
        self.invites.values().filter(move |i| i.to == account)
    }

    /// Pending invites sent by `account`, in id order.
    pub fn invites_from(&self, account: AccountId) -> impl Iterator<Item = &Invite> + '_ {
        self.invites.values().filter(move |i| i.from == account)
    }

    // ----- operations ----------------------------------------------------------------------

    /// `from` invites `to` into their party (or to form one). Returns the new invite's id and
    /// the events to broadcast. If a timed-out invite for the same `(from, to)` pair was still
    /// stored, it is replaced and its `InviteExpired` event comes first, so do not assume
    /// `InviteSent` is `events[0]`; use the returned id.
    ///
    /// Errors: [`SelfInvite`](PartyError::SelfInvite), [`AlreadyInParty`](PartyError::AlreadyInParty)
    /// (invitee), [`NotLeader`](PartyError::NotLeader), [`PartyFull`](PartyError::PartyFull),
    /// [`DuplicateInvite`](PartyError::DuplicateInvite) (a live invite for the pair exists).
    pub fn invite(
        &mut self,
        from: AccountId,
        to: AccountId,
        now: Tick,
    ) -> Result<(InviteId, Vec<PartyEvent>), PartyError> {
        if from == to {
            return Err(PartyError::SelfInvite);
        }
        if self.member_party.contains_key(&to) {
            return Err(PartyError::AlreadyInParty { account: to });
        }
        if let Some(party) = self.party_ref(from)? {
            if party.leader != from {
                return Err(PartyError::NotLeader { account: from });
            }
            if party.members.len() >= self.config.max_size() {
                return Err(PartyError::PartyFull {
                    party: party.id,
                    max_size: self.config.max_size(),
                });
            }
        }
        let stale = match self.invites.values().find(|i| i.from == from && i.to == to) {
            Some(existing) if existing.is_expired(now) => Some(*existing),
            Some(_) => return Err(PartyError::DuplicateInvite { from, to }),
            None => None,
        };
        let id = InviteId(self.next_invite);
        let next_invite = self
            .next_invite
            .checked_add(1)
            .ok_or(PartyError::IdSpaceExhausted)?;

        // Everything validated; from here on nothing can fail.
        let mut events = Vec::with_capacity(2);
        if let Some(old) = stale {
            self.invites.remove(&old.id);
            events.push(PartyEvent::InviteExpired {
                invite: old.id,
                from,
                to,
            });
        }
        self.next_invite = next_invite;
        let expires_at = now.saturating_add(self.config.invite_ttl());
        self.invites.insert(
            id,
            Invite {
                id,
                from,
                to,
                created_at: now,
                expires_at,
            },
        );
        events.push(PartyEvent::InviteSent {
            invite: id,
            from,
            to,
            expires_at,
        });
        Ok((id, events))
    }

    /// `by` accepts `invite`. `by` must be the invitee: invite ids are sequential and
    /// guessable, so the manager (not the caller) enforces the addressee.
    ///
    /// Events: `PartyFormed` (inviter was solo) or `MemberJoined` (inviter leads a party), then
    /// an `InviteCancelled` for every other invite the join invalidates, in invite-id order.
    /// The accepted invite itself is reported only through the `invite` field of the first event.
    ///
    /// Errors: [`UnknownInvite`](PartyError::UnknownInvite), [`NotInvitee`](PartyError::NotInvitee),
    /// [`InviteExpired`](PartyError::InviteExpired) (the invite stays stored until
    /// [`expire`](Self::expire) reaps it), [`AlreadyInParty`](PartyError::AlreadyInParty),
    /// [`PartyFull`](PartyError::PartyFull) (the invite stays pending, so it can still succeed
    /// if a slot frees up before it expires).
    pub fn accept(
        &mut self,
        invite: InviteId,
        by: AccountId,
        now: Tick,
    ) -> Result<Vec<PartyEvent>, PartyError> {
        let inv = *self
            .invites
            .get(&invite)
            .ok_or(PartyError::UnknownInvite { invite })?;
        if inv.to != by {
            return Err(PartyError::NotInvitee {
                invite,
                account: by,
            });
        }
        if inv.is_expired(now) {
            return Err(PartyError::InviteExpired { invite });
        }
        if self.member_party.contains_key(&by) {
            return Err(PartyError::AlreadyInParty { account: by });
        }
        let (target, next_party) = match self.party_ref(inv.from)? {
            None => {
                let id = PartyId(self.next_party);
                let next = self
                    .next_party
                    .checked_add(1)
                    .ok_or(PartyError::IdSpaceExhausted)?;
                (Join::NewParty(id), next)
            }
            Some(party) => {
                if party.leader != inv.from {
                    return Err(PartyError::InviterCannotInvite { invite });
                }
                if party.members.len() >= self.config.max_size() {
                    return Err(PartyError::PartyFull {
                        party: party.id,
                        max_size: self.config.max_size(),
                    });
                }
                (Join::Existing(party.id), self.next_party)
            }
        };

        // Validated. The only fallible step below (`get_mut`) runs before any mutation.
        let mut events = Vec::new();
        // Accounts that are in a party now but were not before this call.
        let mut newly_in_party = vec![by];
        match target {
            Join::NewParty(id) => {
                let members = vec![inv.from, by];
                events.push(PartyEvent::PartyFormed {
                    party: id,
                    invite,
                    leader: inv.from,
                    members: members.clone(),
                });
                self.parties.insert(
                    id,
                    Party {
                        id,
                        leader: inv.from,
                        members,
                    },
                );
                self.member_party.insert(inv.from, id);
                self.member_party.insert(by, id);
                newly_in_party.push(inv.from);
            }
            Join::Existing(id) => {
                let party = self
                    .parties
                    .get_mut(&id)
                    .ok_or_else(|| PartyError::corrupt("member index names a missing party"))?;
                party.members.push(by);
                events.push(PartyEvent::MemberJoined {
                    party: id,
                    invite,
                    account: by,
                    leader: party.leader,
                    members: party.members.clone(),
                });
                self.member_party.insert(by, id);
            }
        }
        self.next_party = next_party;
        self.invites.remove(&invite);
        self.cancel_invites(&mut events, |i| {
            if newly_in_party.contains(&i.to) {
                Some(InviteCancelReason::InviteeJoinedParty)
            } else if i.from == by {
                // `by` is a plain member now; members cannot invite.
                Some(InviteCancelReason::InviterCannotInvite)
            } else {
                None
            }
        });
        Ok(events)
    }

    /// `by` declines `invite`. Succeeds even if the invite has timed out but was not reaped
    /// yet (declining takes no clock). Errors: `UnknownInvite`, `NotInvitee`.
    pub fn decline(
        &mut self,
        invite: InviteId,
        by: AccountId,
    ) -> Result<Vec<PartyEvent>, PartyError> {
        let inv = *self
            .invites
            .get(&invite)
            .ok_or(PartyError::UnknownInvite { invite })?;
        if inv.to != by {
            return Err(PartyError::NotInvitee {
                invite,
                account: by,
            });
        }
        self.invites.remove(&invite);
        Ok(vec![PartyEvent::InviteDeclined {
            invite,
            from: inv.from,
            to: inv.to,
        }])
    }

    /// `member` leaves their party.
    ///
    /// * Last member: only `Disbanded` (no `MemberLeft`).
    /// * Leader with others remaining: `LeaderChanged` (longest-standing remaining member), then
    ///   `MemberLeft`.
    /// * Anyone else: `MemberLeft`.
    ///
    /// Invites the leaver had sent are cancelled afterwards (`InviterLeftParty`).
    /// Errors: [`NotInParty`](PartyError::NotInParty).
    pub fn leave(&mut self, member: AccountId) -> Result<Vec<PartyEvent>, PartyError> {
        let party = self
            .party_ref(member)?
            .ok_or(PartyError::NotInParty { account: member })?;
        if !party.contains(member) {
            return Err(PartyError::corrupt(
                "member index names a party that does not list the member",
            ));
        }
        let pid = party.id;
        let roster = party.members.clone();
        let was_leader = party.leader == member;

        let mut events = Vec::new();
        if roster.len() == 1 {
            events.push(PartyEvent::Disbanded {
                party: pid,
                members: roster,
                reason: DisbandReason::LastMemberLeft,
            });
            self.parties.remove(&pid);
            self.member_party.remove(&member);
        } else {
            let successor = if was_leader {
                // Join order is seniority, so the first other entry is the longest-standing.
                Some(
                    roster
                        .iter()
                        .copied()
                        .find(|m| *m != member)
                        .ok_or_else(|| {
                            PartyError::corrupt("party has no member besides its leader")
                        })?,
                )
            } else {
                None
            };
            let remaining: Vec<AccountId> =
                roster.iter().copied().filter(|m| *m != member).collect();
            let party = self
                .parties
                .get_mut(&pid)
                .ok_or_else(|| PartyError::corrupt("member index names a missing party"))?;
            if let Some(new_leader) = successor {
                events.push(PartyEvent::LeaderChanged {
                    party: pid,
                    old_leader: member,
                    new_leader,
                    reason: LeaderChangeReason::LeaderLeft,
                    members: roster,
                });
                party.leader = new_leader;
            }
            events.push(PartyEvent::MemberLeft {
                party: pid,
                account: member,
                reason: LeaveReason::Left,
                members: remaining.clone(),
            });
            party.members = remaining;
            self.member_party.remove(&member);
        }
        self.cancel_invites(&mut events, |i| {
            (i.from == member).then_some(InviteCancelReason::InviterLeftParty)
        });
        Ok(events)
    }

    /// `leader` removes `member` from their party. Event: `MemberLeft { reason: Kicked }`.
    /// Errors: `NotInParty` (actor), `NotLeader`, `CannotTargetSelf` (leave instead),
    /// `TargetNotInParty`.
    pub fn kick(
        &mut self,
        leader: AccountId,
        member: AccountId,
    ) -> Result<Vec<PartyEvent>, PartyError> {
        let party = self
            .party_ref(leader)?
            .ok_or(PartyError::NotInParty { account: leader })?;
        if party.leader != leader {
            return Err(PartyError::NotLeader { account: leader });
        }
        if member == leader {
            return Err(PartyError::CannotTargetSelf { account: leader });
        }
        if !party.contains(member) {
            return Err(PartyError::TargetNotInParty { account: member });
        }
        let pid = party.id;
        let party = self
            .parties
            .get_mut(&pid)
            .ok_or_else(|| PartyError::corrupt("member index names a missing party"))?;
        party.members.retain(|m| *m != member);
        let members = party.members.clone();
        self.member_party.remove(&member);
        Ok(vec![PartyEvent::MemberLeft {
            party: pid,
            account: member,
            reason: LeaveReason::Kicked,
            members,
        }])
    }

    /// `leader` hands leadership to `member`. Event: `LeaderChanged { reason: Promoted }`, then
    /// cancellations (`InviterCannotInvite`) of the old leader's pending invites.
    /// Errors: `NotInParty` (actor), `NotLeader`, `AlreadyLeader`, `TargetNotInParty`.
    pub fn promote(
        &mut self,
        leader: AccountId,
        member: AccountId,
    ) -> Result<Vec<PartyEvent>, PartyError> {
        let party = self
            .party_ref(leader)?
            .ok_or(PartyError::NotInParty { account: leader })?;
        if party.leader != leader {
            return Err(PartyError::NotLeader { account: leader });
        }
        if member == leader {
            return Err(PartyError::AlreadyLeader { account: leader });
        }
        if !party.contains(member) {
            return Err(PartyError::TargetNotInParty { account: member });
        }
        let pid = party.id;
        let party = self
            .parties
            .get_mut(&pid)
            .ok_or_else(|| PartyError::corrupt("member index names a missing party"))?;
        party.leader = member;
        let mut events = vec![PartyEvent::LeaderChanged {
            party: pid,
            old_leader: leader,
            new_leader: member,
            reason: LeaderChangeReason::Promoted,
            members: party.members.clone(),
        }];
        self.cancel_invites(&mut events, |i| {
            (i.from == leader).then_some(InviteCancelReason::InviterCannotInvite)
        });
        Ok(events)
    }

    /// Removes every invite that has timed out at `now` and announces each with
    /// `InviteExpired`, in invite-id order. Call it every tick (or before handling a batch of
    /// player commands). Takes `Result` only so every operation shares one signature; it cannot
    /// fail today.
    pub fn expire(&mut self, now: Tick) -> Result<Vec<PartyEvent>, PartyError> {
        let dead: Vec<Invite> = self
            .invites
            .values()
            .filter(|i| i.is_expired(now))
            .copied()
            .collect();
        let mut events = Vec::with_capacity(dead.len());
        for inv in dead {
            self.invites.remove(&inv.id);
            events.push(PartyEvent::InviteExpired {
                invite: inv.id,
                from: inv.from,
                to: inv.to,
            });
        }
        Ok(events)
    }

    // ----- integrity -----------------------------------------------------------------------

    /// Checks every internal invariant. Cheap enough to call after loading persisted state or
    /// after each operation in tests. Deserialization runs it automatically.
    ///
    /// Invariants: ids are below their counters; no empty party; no party above `max_size`; the
    /// leader is a member; no account is in two parties; the member index matches the parties;
    /// every invite has `from != to`, an invitee in no party, a solo-or-leader inviter, and is
    /// the only invite for its `(from, to)` pair.
    pub fn validate(&self) -> Result<(), PartyError> {
        let mut seen: BTreeMap<AccountId, PartyId> = BTreeMap::new();
        for (key, party) in &self.parties {
            if *key != party.id {
                return Err(PartyError::corrupt("party stored under a different id"));
            }
            if party.id.0 >= self.next_party {
                return Err(PartyError::corrupt(
                    "party id is not below the party counter",
                ));
            }
            if party.members.is_empty() {
                return Err(PartyError::corrupt("party has no members"));
            }
            if party.members.len() > self.config.max_size() {
                return Err(PartyError::corrupt("party is larger than max_size"));
            }
            if !party.members.contains(&party.leader) {
                return Err(PartyError::corrupt("party leader is not a member"));
            }
            for member in &party.members {
                if seen.insert(*member, party.id).is_some() {
                    return Err(PartyError::corrupt(
                        "account appears in more than one party slot",
                    ));
                }
            }
        }
        if seen != self.member_party {
            return Err(PartyError::corrupt(
                "member index does not match the parties",
            ));
        }
        let mut pairs = std::collections::BTreeSet::new();
        for (key, inv) in &self.invites {
            if *key != inv.id {
                return Err(PartyError::corrupt("invite stored under a different id"));
            }
            if inv.id.0 >= self.next_invite {
                return Err(PartyError::corrupt(
                    "invite id is not below the invite counter",
                ));
            }
            if inv.from == inv.to {
                return Err(PartyError::corrupt("invite from an account to itself"));
            }
            if inv.expires_at < inv.created_at {
                return Err(PartyError::corrupt("invite expires before it was created"));
            }
            if self.member_party.contains_key(&inv.to) {
                return Err(PartyError::corrupt(
                    "invite addressed to an account already in a party",
                ));
            }
            if let Some(party) = self.party_of(inv.from) {
                if party.leader != inv.from {
                    return Err(PartyError::corrupt("invite sent by a non-leader member"));
                }
            }
            if !pairs.insert((inv.from, inv.to)) {
                return Err(PartyError::corrupt(
                    "two invites for the same (from, to) pair",
                ));
            }
        }
        Ok(())
    }

    // ----- helpers -------------------------------------------------------------------------

    /// The party of `account`, `Ok(None)` if solo, `Err` only if the index dangles.
    fn party_ref(&self, account: AccountId) -> Result<Option<&Party>, PartyError> {
        match self.member_party.get(&account) {
            None => Ok(None),
            Some(id) => self
                .parties
                .get(id)
                .map(Some)
                .ok_or_else(|| PartyError::corrupt("member index names a missing party")),
        }
    }

    /// Removes every invite for which `why` returns a reason and appends one `InviteCancelled`
    /// per removed invite, in invite-id order. A single predicate gives each invite at most one
    /// reason, so an invite hit by several causes is still announced exactly once.
    fn cancel_invites(
        &mut self,
        events: &mut Vec<PartyEvent>,
        why: impl Fn(&Invite) -> Option<InviteCancelReason>,
    ) {
        let doomed: Vec<(Invite, InviteCancelReason)> = self
            .invites
            .values()
            .filter_map(|i| why(i).map(|r| (*i, r)))
            .collect();
        for (inv, reason) in doomed {
            self.invites.remove(&inv.id);
            events.push(PartyEvent::InviteCancelled {
                invite: inv.id,
                from: inv.from,
                to: inv.to,
                reason,
            });
        }
    }

    fn from_state(state: ManagerState) -> Result<Self, PartyError> {
        let mut parties = BTreeMap::new();
        let mut member_party = BTreeMap::new();
        for party in state.parties {
            for member in &party.members {
                if member_party.insert(*member, party.id).is_some() {
                    return Err(PartyError::corrupt(
                        "account appears in more than one party slot",
                    ));
                }
            }
            if parties.insert(party.id, party).is_some() {
                return Err(PartyError::corrupt("duplicate party id"));
            }
        }
        let mut invites = BTreeMap::new();
        for inv in state.invites {
            if invites.insert(inv.id, inv).is_some() {
                return Err(PartyError::corrupt("duplicate invite id"));
            }
        }
        let manager = Self {
            config: state.config,
            next_party: state.next_party,
            next_invite: state.next_invite,
            parties,
            invites,
            member_party,
        };
        manager.validate()?;
        Ok(manager)
    }
}

/// Wire shape of a persisted manager: plain sequences in id order (no map keys), so any serde
/// format can store it. Both views below must keep identical field names.
#[derive(Serialize)]
struct ManagerStateRef<'a> {
    config: &'a PartyConfig,
    next_party: u64,
    next_invite: u64,
    parties: Vec<&'a Party>,
    invites: Vec<&'a Invite>,
}

#[derive(Deserialize)]
struct ManagerState {
    config: PartyConfig,
    next_party: u64,
    next_invite: u64,
    parties: Vec<Party>,
    invites: Vec<Invite>,
}

impl Serialize for PartyManager {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        ManagerStateRef {
            config: &self.config,
            next_party: self.next_party,
            next_invite: self.next_invite,
            parties: self.parties.values().collect(),
            invites: self.invites.values().collect(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for PartyManager {
    /// Rebuilds the derived index and rejects state that violates any invariant.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let state = ManagerState::deserialize(deserializer)?;
        PartyManager::from_state(state).map_err(serde::de::Error::custom)
    }
}
