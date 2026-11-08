//! Game-agnostic party management and story-phase instance selection.
//!
//! Two independent, pure, deterministic pieces (no networking, no clocks, no randomness; the
//! caller passes `now` and routes events). Both are plain data structures that compare equal
//! after a serde round trip and reject corrupt persisted state instead of panicking.
//!
//! * **Parties** ([`PartyManager`]): invites with expiry, accept/decline, leave, kick, promote,
//!   size cap per game ([`PartyConfig`]). Every operation is atomic (an `Err` changes nothing)
//!   and returns [`PartyEvent`]s describing exactly what clients should be told; see
//!   [`event`] for the replay contract and [`PartyEvent::audience`] for routing.
//! * **Phasing** ([`InstanceSelector`], [`InstanceRegistry`]): solo players land in
//!   `(area, own phase)`, a party shares `(area, leader's phase)` while every member keeps
//!   their own progression. The registry creates instances on demand, tracks occupants and
//!   releases an instance when it empties. See [`phasing`].
//!
//! Core never interprets a [`PhaseKey`](fmv_core::PhaseKey): it is opaque and only compared for
//! equality. Adapters compute it from per-game progression.

#![forbid(unsafe_code)]

pub mod config;
pub mod error;
pub mod event;
pub mod ids;
pub mod instance;
pub mod party;
pub mod phasing;

pub use config::{PartyConfig, DEFAULT_INVITE_TTL, DEFAULT_MAX_PARTY_SIZE, MIN_PARTY_SIZE};
pub use error::PartyError;
pub use event::{DisbandReason, InviteCancelReason, LeaderChangeReason, LeaveReason, PartyEvent};
pub use ids::{InstanceId, InviteId, PartyId, Tick};
pub use instance::{Instance, InstanceEvent, InstanceRegistry};
pub use party::{Invite, Party, PartyManager};
pub use phasing::{
    InstanceKey, InstanceSelector, MemberPlacement, PartyPlacement, PhaseMap, PhasingError,
    Placement,
};
