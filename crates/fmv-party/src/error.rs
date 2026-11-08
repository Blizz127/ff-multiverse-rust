//! Errors returned by [`PartyManager`](crate::PartyManager) operations.

use crate::ids::{InviteId, PartyId};
use fmv_core::AccountId;
use serde::{Deserialize, Serialize};

/// Why a party operation was refused. A refused operation never changes any state.
///
/// Serializable so a server can answer a client request with the exact typed reason.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
pub enum PartyError {
    #[error("invalid party configuration: {0}")]
    InvalidConfig(String),
    #[error("an account cannot invite itself")]
    SelfInvite,
    /// The invitee (or the accepting account) already belongs to a party.
    #[error("account {account:?} is already in a party")]
    AlreadyInParty { account: AccountId },
    /// Only a party's leader (or a solo player) may invite.
    #[error("account {account:?} is a party member but not its leader")]
    NotLeader { account: AccountId },
    /// The acting account is not in any party.
    #[error("account {account:?} is not in a party")]
    NotInParty { account: AccountId },
    /// The kick/promote target is not a member of the acting leader's party.
    #[error("account {account:?} is not a member of that party")]
    TargetNotInParty { account: AccountId },
    #[error("a pending invite from {from:?} to {to:?} already exists")]
    DuplicateInvite { from: AccountId, to: AccountId },
    #[error("{party} is full (max {max_size} members)")]
    PartyFull { party: PartyId, max_size: usize },
    #[error("unknown invite {invite}")]
    UnknownInvite { invite: InviteId },
    #[error("{invite} has expired")]
    InviteExpired { invite: InviteId },
    /// `accept`/`decline` was called by someone other than the invitee.
    #[error("{invite} is not addressed to account {account:?}")]
    NotInvitee {
        invite: InviteId,
        account: AccountId,
    },
    /// The inviter is currently a non-leader member, so the invite can no longer be honoured.
    /// Unreachable while the manager's invariants hold (such invites are cancelled eagerly);
    /// kept as a defensive check.
    #[error("the inviter of {invite} is no longer able to invite")]
    InviterCannotInvite { invite: InviteId },
    /// A leader cannot kick themselves; they must `leave`.
    #[error("account {account:?} cannot target itself with this operation")]
    CannotTargetSelf { account: AccountId },
    /// `promote` named the current leader.
    #[error("account {account:?} is already the leader")]
    AlreadyLeader { account: AccountId },
    #[error("id space exhausted")]
    IdSpaceExhausted,
    /// Internal invariants do not hold. Returned by `validate`/deserialization on bad state and,
    /// defensively, instead of panicking if an index ever disagrees with the data.
    #[error("corrupt party state: {0}")]
    CorruptState(String),
}

impl PartyError {
    pub(crate) fn corrupt(what: impl Into<String>) -> Self {
        PartyError::CorruptState(what.into())
    }
}
