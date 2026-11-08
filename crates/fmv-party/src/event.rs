//! Events describing exactly what clients should be told about a party-state change.
//!
//! # Contract
//!
//! * **Replayable deltas.** Applying the events of every successful operation, in order, to a
//!   client-side mirror reproduces the manager's state: parties (leader, members) and pending
//!   invites. Every intermediate state is valid: a party always contains its leader, so on a
//!   leader leaving, [`PartyEvent::LeaderChanged`] comes *before* the [`PartyEvent::MemberLeft`]
//!   of the old leader.
//! * **Self-addressed.** Every event carries what [`PartyEvent::audience`] needs, so the server
//!   can fan events out without consulting the manager again (the party may already be gone).
//!   Party events carry the roster *after* the event, oldest member first, which also lets a
//!   brand-new member build the whole party from its first event.
//! * **One terminal event per invite.** Each invite that was announced with
//!   [`PartyEvent::InviteSent`] ends with exactly one of `InviteDeclined`, `InviteExpired`,
//!   `InviteCancelled`, or the `invite` field of `PartyFormed` / `MemberJoined`.
//! * **`Disbanded` replaces the final `MemberLeft`.** When the last member leaves, the only
//!   event for it is `Disbanded`; there is no separate `MemberLeft`.

use crate::ids::{InviteId, PartyId, Tick};
use fmv_core::AccountId;
use serde::{Deserialize, Serialize};

/// Why a member is no longer in the party.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LeaveReason {
    /// The member chose to leave.
    Left,
    /// The leader removed the member.
    Kicked,
}

/// Why leadership moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LeaderChangeReason {
    /// The leader explicitly promoted another member.
    Promoted,
    /// The leader left; the longest-standing remaining member took over.
    LeaderLeft,
}

/// Why a party ceased to exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DisbandReason {
    /// The last remaining member left.
    LastMemberLeft,
}

/// Why a pending invite was withdrawn by the system (as opposed to declined or expired).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum InviteCancelReason {
    /// The invitee joined (or formed) a party, so invites to them are moot.
    InviteeJoinedParty,
    /// The inviter left their party or it disbanded.
    InviterLeftParty,
    /// The inviter became a non-leader member (joined another party or was demoted by a
    /// promotion), so they may no longer invite.
    InviterCannotInvite,
}

/// One thing clients should be told. See the [module docs](self) for the contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PartyEvent {
    /// An invite was created. Audience: inviter and invitee.
    InviteSent {
        invite: InviteId,
        from: AccountId,
        to: AccountId,
        expires_at: Tick,
    },
    /// The invitee declined. Audience: inviter and invitee.
    InviteDeclined {
        invite: InviteId,
        from: AccountId,
        to: AccountId,
    },
    /// The invite timed out (or was replaced by a fresh one after timing out). Audience: both.
    InviteExpired {
        invite: InviteId,
        from: AccountId,
        to: AccountId,
    },
    /// The system withdrew the invite as a side effect of another change. Audience: both.
    InviteCancelled {
        invite: InviteId,
        from: AccountId,
        to: AccountId,
        reason: InviteCancelReason,
    },
    /// An invite from a solo player was accepted, creating a new party. `members` is
    /// `[leader, invitee]`; `invite` is the accepted invite. Audience: `members`.
    PartyFormed {
        party: PartyId,
        invite: InviteId,
        leader: AccountId,
        members: Vec<AccountId>,
    },
    /// An invite into an existing party was accepted. `members` is the roster after the join
    /// (oldest first, `account` last); `leader` is included so the joiner can mirror the party.
    /// Audience: `members`.
    MemberJoined {
        party: PartyId,
        invite: InviteId,
        account: AccountId,
        leader: AccountId,
        members: Vec<AccountId>,
    },
    /// A member left or was kicked and the party continues. `members` is the roster *after*
    /// the removal. Audience: `members` plus `account` (who must learn it is out).
    MemberLeft {
        party: PartyId,
        account: AccountId,
        reason: LeaveReason,
        members: Vec<AccountId>,
    },
    /// Leadership moved. `members` is the roster at that moment; on `LeaderLeft` it still
    /// contains `old_leader`, whose `MemberLeft` follows immediately. Audience: `members`.
    LeaderChanged {
        party: PartyId,
        old_leader: AccountId,
        new_leader: AccountId,
        reason: LeaderChangeReason,
        members: Vec<AccountId>,
    },
    /// The party no longer exists. `members` is the roster that was dissolved, so the audience
    /// is never empty. Replaces the final `MemberLeft`.
    Disbanded {
        party: PartyId,
        members: Vec<AccountId>,
        reason: DisbandReason,
    },
}

impl PartyEvent {
    /// Accounts whose clients must receive this event, in a deterministic order.
    pub fn audience(&self) -> Vec<AccountId> {
        match self {
            PartyEvent::InviteSent { from, to, .. }
            | PartyEvent::InviteDeclined { from, to, .. }
            | PartyEvent::InviteExpired { from, to, .. }
            | PartyEvent::InviteCancelled { from, to, .. } => vec![*from, *to],
            PartyEvent::PartyFormed { members, .. }
            | PartyEvent::MemberJoined { members, .. }
            | PartyEvent::LeaderChanged { members, .. }
            | PartyEvent::Disbanded { members, .. } => members.clone(),
            PartyEvent::MemberLeft {
                account, members, ..
            } => {
                let mut who = members.clone();
                who.push(*account);
                who
            }
        }
    }

    /// The party this event is about, or `None` for pure invite events.
    pub fn party(&self) -> Option<PartyId> {
        match self {
            PartyEvent::PartyFormed { party, .. }
            | PartyEvent::MemberJoined { party, .. }
            | PartyEvent::MemberLeft { party, .. }
            | PartyEvent::LeaderChanged { party, .. }
            | PartyEvent::Disbanded { party, .. } => Some(*party),
            _ => None,
        }
    }

    /// The invite this event is about, if any (including the invite consumed by an accept).
    pub fn invite(&self) -> Option<InviteId> {
        match self {
            PartyEvent::InviteSent { invite, .. }
            | PartyEvent::InviteDeclined { invite, .. }
            | PartyEvent::InviteExpired { invite, .. }
            | PartyEvent::InviteCancelled { invite, .. }
            | PartyEvent::PartyFormed { invite, .. }
            | PartyEvent::MemberJoined { invite, .. } => Some(*invite),
            _ => None,
        }
    }
}
