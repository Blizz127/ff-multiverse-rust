//! Identifier and time types used by the party and phasing modules.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Caller-supplied, monotonically non-decreasing time in caller-chosen units.
///
/// This crate never reads a clock. Whoever drives it (normally the server tick loop) passes
/// `now` into every time-dependent call and expresses durations such as
/// [`PartyConfig::invite_ttl`](crate::PartyConfig::invite_ttl) in the *same* unit (server ticks
/// or milliseconds, the crate does not care). A `now` that goes backwards never panics; it just
/// means fewer invites look expired.
pub type Tick = u64;

macro_rules! id_type {
    ($(#[$meta:meta])* $name:ident, $prefix:literal) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        pub struct $name(pub u64);

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!($prefix, "#{}"), self.0)
            }
        }
    };
}

id_type!(
    /// Identifies one party for its whole life. Allocated from a persisted counter and never
    /// reused, even after the party disbands.
    PartyId,
    "party"
);
id_type!(
    /// Identifies one pending invite. Allocated from a persisted counter and never reused.
    ///
    /// Ids are sequential and therefore guessable; [`PartyManager::accept`] and
    /// [`PartyManager::decline`] take the acting account so a guessed id is useless to anyone
    /// but the invitee.
    ///
    /// [`PartyManager::accept`]: crate::PartyManager::accept
    /// [`PartyManager::decline`]: crate::PartyManager::decline
    InviteId,
    "invite"
);
id_type!(
    /// Identifies one live area instance in an [`InstanceRegistry`](crate::InstanceRegistry).
    /// Never reused within one registry.
    InstanceId,
    "instance"
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_is_prefixed() {
        assert_eq!(PartyId(7).to_string(), "party#7");
        assert_eq!(InviteId(1).to_string(), "invite#1");
        assert_eq!(InstanceId(0).to_string(), "instance#0");
    }
}
