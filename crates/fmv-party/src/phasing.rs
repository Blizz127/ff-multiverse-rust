//! Story phasing (plan section 6): which area instance a player lands in.
//!
//! The adapter turns a player's *per-game progression* into an opaque [`PhaseKey`] for one area.
//! This module only compares keys for equality and never reads, writes or interprets
//! progression, so it cannot leak story state between players or games.
//!
//! # Rules
//!
//! * **Solo**: the instance is `(area, own phase)`.
//! * **Party**: the instance is `(area, leader's phase)` for *every* member. A member whose own
//!   progression is behind or ahead of the leader's still joins the leader's instance, and they
//!   **keep their own progression**: nothing here changes it. Core cannot tell "behind" from
//!   "ahead" (keys are opaque, equality only), only "same" from "different".
//! * **Story advancement** inside a shared instance is the hook
//!   [`InstanceSelector::story_advance_allowed`]: by default a member may advance the story only
//!   when their own phase equals the leader's. The adapter/server consults the per-member
//!   `story_advance_allowed` flag before applying story events to that member. The policy is
//!   deliberately a single function so it can be made configurable later without changing the
//!   shape of [`Placement`] / [`MemberPlacement`].
//! * **M1**: the chosen field has a single "free roam" phase, so every key's phase is the same
//!   (for example `PhaseKey::default()`) and every flag is `true`.

use crate::ids::PartyId;
use crate::party::Party;
use fmv_core::{AccountId, AreaId, PhaseKey};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Each account's *own* [`PhaseKey`] for one area, as computed by the adapter from that
/// account's progression. Only the entries a selection needs have to be present.
pub type PhaseMap = BTreeMap<AccountId, PhaseKey>;

/// Identity of one area instance: the area plus the story phase it is running.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct InstanceKey {
    pub area: AreaId,
    pub phase: PhaseKey,
}

impl InstanceKey {
    pub fn new(area: AreaId, phase: PhaseKey) -> Self {
        Self { area, phase }
    }
}

/// Why a selection could not be made. Selection never changes any state.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
pub enum PhasingError {
    /// The caller did not supply a phase key for an account the selection needs.
    #[error("no phase key supplied for account {account:?}")]
    MissingPhase { account: AccountId },
    /// `player` was named but is not a member of the given party.
    #[error("account {account:?} is not a member of {party}")]
    NotInParty { account: AccountId, party: PartyId },
    /// The account is not placed in any instance.
    #[error("account {account:?} is not in any instance")]
    NotInInstance { account: AccountId },
    #[error("instance id space exhausted")]
    IdSpaceExhausted,
    /// Registry state failed validation (only from `validate`/deserialization).
    #[error("corrupt instance registry: {0}")]
    CorruptState(String),
}

/// Where one player goes, and whether story events may advance them there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Placement {
    pub account: AccountId,
    pub key: InstanceKey,
    /// `Some` when the party rule chose the key (including for the leader themselves).
    pub party: Option<PartyId>,
    /// Whose phase chose the key: the player when solo, the leader when in a party.
    pub phase_owner: AccountId,
    /// Default policy: the player's own phase equals the phase that chose the instance.
    /// Always `true` when solo and for the leader.
    pub story_advance_allowed: bool,
}

/// One member's slice of a [`PartyPlacement`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberPlacement {
    pub account: AccountId,
    pub story_advance_allowed: bool,
}

/// Where a whole party goes: one shared key, one flag per member (join order).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartyPlacement {
    pub party: PartyId,
    pub leader: AccountId,
    pub key: InstanceKey,
    pub members: Vec<MemberPlacement>,
}

/// Stateless instance selection. See the [module docs](self) for the rules.
pub struct InstanceSelector;

impl InstanceSelector {
    /// The default story-advancement policy: a member may advance the story only when their own
    /// phase equals the leader's. Opaque keys mean "behind" and "ahead" look the same here
    /// (both `false`). Documented as the single place to make this configurable later.
    pub fn story_advance_allowed(member_phase: &PhaseKey, leader_phase: &PhaseKey) -> bool {
        member_phase == leader_phase
    }

    /// Selection for one player entering `area`.
    ///
    /// `party` is the player's party, if any. `phases` must contain the player's own phase and,
    /// when in a party, the leader's; other members' entries are ignored, so the server only
    /// needs the progression of the people involved.
    pub fn select(
        area: &AreaId,
        player: AccountId,
        party: Option<&Party>,
        phases: &PhaseMap,
    ) -> Result<Placement, PhasingError> {
        match party {
            None => {
                let own = phase_of(phases, player)?;
                Ok(Placement {
                    account: player,
                    key: InstanceKey::new(area.clone(), own.clone()),
                    party: None,
                    phase_owner: player,
                    story_advance_allowed: true,
                })
            }
            Some(party) => {
                if !party.contains(player) {
                    return Err(PhasingError::NotInParty {
                        account: player,
                        party: party.id,
                    });
                }
                let own = phase_of(phases, player)?;
                let leader_phase = phase_of(phases, party.leader)?;
                Ok(Placement {
                    account: player,
                    key: InstanceKey::new(area.clone(), leader_phase.clone()),
                    party: Some(party.id),
                    phase_owner: party.leader,
                    story_advance_allowed: Self::story_advance_allowed(own, leader_phase),
                })
            }
        }
    }

    /// Selection for every member of `party` at once (for example when the leader moves the
    /// party to a new area). `phases` must contain every member's own phase.
    pub fn select_party(
        area: &AreaId,
        party: &Party,
        phases: &PhaseMap,
    ) -> Result<PartyPlacement, PhasingError> {
        let leader_phase = phase_of(phases, party.leader)?;
        let mut members = Vec::with_capacity(party.members.len());
        for &account in &party.members {
            let own = phase_of(phases, account)?;
            members.push(MemberPlacement {
                account,
                story_advance_allowed: Self::story_advance_allowed(own, leader_phase),
            });
        }
        Ok(PartyPlacement {
            party: party.id,
            leader: party.leader,
            key: InstanceKey::new(area.clone(), leader_phase.clone()),
            members,
        })
    }
}

fn phase_of(phases: &PhaseMap, account: AccountId) -> Result<&PhaseKey, PhasingError> {
    phases
        .get(&account)
        .ok_or(PhasingError::MissingPhase { account })
}
