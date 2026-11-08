//! Per-game party configuration.

use crate::error::PartyError;
use crate::ids::Tick;
use serde::{Deserialize, Serialize};

/// FF7's active party size. Other titles override it through [`PartyConfig::new`]; adapters are
/// the natural owner of that number.
pub const DEFAULT_MAX_PARTY_SIZE: usize = 3;

/// A party needs a leader and at least one other member, so smaller caps make no sense.
pub const MIN_PARTY_SIZE: usize = 2;

/// Default invite lifetime: 60 seconds *if the caller's `now` is in milliseconds*. A server that
/// drives the manager with ticks must build an explicit config, e.g. `30 Hz * 60 s = 1_800`.
pub const DEFAULT_INVITE_TTL: Tick = 60_000;

/// Validated party configuration. Construct with [`PartyConfig::new`] (or `Default`); the fields
/// are private so an invalid config cannot exist, including after deserialization.
///
/// One [`PartyManager`](crate::PartyManager) holds one config. A server that hosts several games
/// with different party caps keeps one manager per game, each built from that game's config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawPartyConfig")]
pub struct PartyConfig {
    max_size: usize,
    invite_ttl: Tick,
}

#[derive(Deserialize)]
struct RawPartyConfig {
    max_size: usize,
    invite_ttl: Tick,
}

impl TryFrom<RawPartyConfig> for PartyConfig {
    type Error = PartyError;
    fn try_from(raw: RawPartyConfig) -> Result<Self, PartyError> {
        PartyConfig::new(raw.max_size, raw.invite_ttl)
    }
}

impl PartyConfig {
    /// `max_size` is the largest party (leader included), at least [`MIN_PARTY_SIZE`].
    /// `invite_ttl` is how many [`Tick`]s an invite stays acceptable, at least 1: an invite
    /// created at `now` can be accepted while `now' < now + invite_ttl`.
    pub fn new(max_size: usize, invite_ttl: Tick) -> Result<Self, PartyError> {
        if max_size < MIN_PARTY_SIZE {
            return Err(PartyError::InvalidConfig(format!(
                "max_size {max_size} is below the minimum of {MIN_PARTY_SIZE}"
            )));
        }
        if invite_ttl == 0 {
            return Err(PartyError::InvalidConfig(
                "invite_ttl must be at least 1".to_string(),
            ));
        }
        Ok(Self {
            max_size,
            invite_ttl,
        })
    }

    /// Largest allowed party, leader included.
    pub fn max_size(&self) -> usize {
        self.max_size
    }

    /// Invite lifetime in caller-chosen [`Tick`] units.
    pub fn invite_ttl(&self) -> Tick {
        self.invite_ttl
    }
}

impl Default for PartyConfig {
    fn default() -> Self {
        Self {
            max_size: DEFAULT_MAX_PARTY_SIZE,
            invite_ttl: DEFAULT_INVITE_TTL,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_matches_ff7() {
        let c = PartyConfig::default();
        assert_eq!(c.max_size(), 3);
        assert_eq!(c.invite_ttl(), DEFAULT_INVITE_TTL);
        assert_eq!(PartyConfig::new(3, DEFAULT_INVITE_TTL).unwrap(), c);
    }

    #[test]
    fn rejects_unusable_values() {
        assert!(matches!(
            PartyConfig::new(0, 10),
            Err(PartyError::InvalidConfig(_))
        ));
        assert!(matches!(
            PartyConfig::new(1, 10),
            Err(PartyError::InvalidConfig(_))
        ));
        assert!(matches!(
            PartyConfig::new(2, 0),
            Err(PartyError::InvalidConfig(_))
        ));
        assert!(PartyConfig::new(2, 1).is_ok());
        assert!(PartyConfig::new(usize::MAX, u64::MAX).is_ok());
    }

    #[test]
    fn deserialization_is_validated() {
        let good: PartyConfig = serde_json::from_str(r#"{"max_size":4,"invite_ttl":9}"#).unwrap();
        assert_eq!((good.max_size(), good.invite_ttl()), (4, 9));
        assert!(serde_json::from_str::<PartyConfig>(r#"{"max_size":1,"invite_ttl":9}"#).is_err());
        assert!(serde_json::from_str::<PartyConfig>(r#"{"max_size":3,"invite_ttl":0}"#).is_err());
    }
}
