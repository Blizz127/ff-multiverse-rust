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
        let Some(player) = self.players.get_mut(&actor) else {
            return PartyOutcome::Gone;
        };
        let account = player.account;
        while player.invites.front().is_some_and(|t| now.saturating_duration_since(*t) >= INVITE_WINDOW) {
            player.invites.pop_front();
        }
        if player.invites.len() >= MAX_INVITES_PER_WINDOW {
            return PartyOutcome::Refused(PartyFailure::RateLimited);
        }
        let Some(target_account) = self.account_of(target) else {
            return PartyOutcome::Refused(PartyFailure::UnknownPlayer);
        };
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
        let clock = self.party_clock(now);
        let result = self.parties.accept(invite, account, clock);
        self.finish(result)
    }

    /// `actor` declines an invite addressed to its account.
    pub fn party_decline(&mut self, actor: EntityId, invite: InviteId) -> PartyOutcome {
        let Some(account) = self.account_of(actor) else {
            return PartyOutcome::Gone;
        };
        let result = self.parties.decline(invite, account);
        self.finish(result)
    }

    /// `actor` leaves its party.
    pub fn party_leave(&mut self, actor: EntityId) -> PartyOutcome {
        let Some(account) = self.account_of(actor) else {
            return PartyOutcome::Gone;
        };
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
