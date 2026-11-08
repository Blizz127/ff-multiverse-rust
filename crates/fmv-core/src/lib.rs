//! Game-agnostic core types shared by server, client, library and every game adapter.
//!
//! Rule: this crate never depends on an adapter. Nothing here may hold game asset bytes;
//! assets are referenced by id + [`Fingerprint`] only.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Which title an area/adapter belongs to. `Fixture` is synthetic test content and is
/// NEVER evidence of support for a real game.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GameId {
    Ff7,
    Ff8,
    Ff9,
    Ff10,
    Ff12,
    Fixture,
}

impl GameId {
    pub fn is_fixture(self) -> bool {
        matches!(self, GameId::Fixture)
    }
    pub fn label(self) -> &'static str {
        match self {
            GameId::Ff7 => "FINAL FANTASY VII",
            GameId::Ff8 => "FINAL FANTASY VIII",
            GameId::Ff9 => "FINAL FANTASY IX",
            GameId::Ff10 => "FINAL FANTASY X",
            GameId::Ff12 => "FINAL FANTASY XII",
            GameId::Fixture => "FIXTURE (synthetic, not a real game)",
        }
    }
}

/// An area within one game, keyed by an adapter-defined string (e.g. FF7 field name).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AreaId {
    pub game: GameId,
    pub key: String,
}

impl fmt::Display for AreaId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}/{}", self.game, self.key)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EntityId(pub u64);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SessionId(pub u64);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AccountId(pub u64);

/// Position in the adapter's own coordinate space (FF7 field units for FF7 fields).
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Pos {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Pos {
    pub const fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }
    pub fn dist_xy(self, o: Pos) -> f32 {
        ((self.x - o.x).powi(2) + (self.y - o.y).powi(2)).sqrt()
    }
    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.z.is_finite()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum AnimState {
    #[default]
    Idle,
    Walk,
    Run,
}

/// blake3 content fingerprint. Exchanged instead of asset bytes.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Fingerprint(pub [u8; 32]);

impl Fingerprint {
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }
    pub fn from_hex(s: &str) -> Result<Self, CoreError> {
        let v = hex::decode(s).map_err(|_| CoreError::BadFingerprint)?;
        let arr: [u8; 32] = v.try_into().map_err(|_| CoreError::BadFingerprint)?;
        Ok(Self(arr))
    }
}
impl fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fingerprint({})", &self.to_hex()[..16])
    }
}
impl Serialize for Fingerprint {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}
impl<'de> Deserialize<'de> for Fingerprint {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Fingerprint::from_hex(&s).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("fingerprint must be 64 hex chars")]
    BadFingerprint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Platform {
    Pc,
    Ps1,
    Synthetic,
    Unknown,
}

/// Where owned content came from. Paths are recorded for the local user only and are
/// never sent to other players (see `ProvenanceSummary`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceRef {
    Disc { index: u8, serial: String },
    Install { path: String },
    Fixture,
}

/// Full edition provenance. Incompatible editions/regions/revisions are never merged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditionProvenance {
    pub game: GameId,
    pub platform: Platform,
    /// e.g. "PC 1998", "Steam 2013", "PS1 original", "PS1 International", "fixture"
    pub edition: String,
    /// e.g. "NTSC-U", "PAL", "NTSC-J", "unknown"
    pub region: String,
    pub language: String,
    /// Disc version / patch level / "unknown"
    pub revision: String,
    pub sources: Vec<SourceRef>,
    /// True if the install had FFNx/7th Heaven or other mods detected. Read-only either way.
    pub modded: bool,
}

impl EditionProvenance {
    /// Shareable summary without local paths.
    pub fn summary(&self) -> ProvenanceSummary {
        ProvenanceSummary {
            game: self.game,
            platform: self.platform,
            edition: self.edition.clone(),
            region: self.region.clone(),
            language: self.language.clone(),
            revision: self.revision.clone(),
            modded: self.modded,
        }
    }
    /// Two provenances may share an instance only when edition/region/revision agree.
    pub fn compatible_with(&self, other: &EditionProvenance) -> bool {
        self.summary().compatible_with(&other.summary())
    }
}

/// Provenance without local paths — safe to put on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvenanceSummary {
    pub game: GameId,
    pub platform: Platform,
    pub edition: String,
    pub region: String,
    pub language: String,
    pub revision: String,
    pub modded: bool,
}

impl ProvenanceSummary {
    pub fn compatible_with(&self, o: &ProvenanceSummary) -> bool {
        self.game == o.game
            && self.platform == o.platform
            && self.edition == o.edition
            && self.region == o.region
            && self.revision == o.revision
    }
}

/// Opaque per-adapter phase key. Core compares for equality only; adapters interpret.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct PhaseKey(pub Vec<u8>);

/// Persistent avatar appearance: references only, never mesh/texture bytes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AvatarAppearance {
    pub display_name: String,
    pub tint: [u8; 3],
    /// Adapter-defined appearance id for the current game (e.g. an FF7 field model name).
    pub appearance_ref: Option<String>,
}

impl AvatarAppearance {
    pub const MAX_NAME: usize = 24;
    /// Trim, strip control chars and clamp length. Returns None if empty after cleaning.
    pub fn sanitize_name(raw: &str) -> Option<String> {
        let s: String = raw
            .chars()
            .filter(|c| !c.is_control())
            .take(Self::MAX_NAME)
            .collect::<String>()
            .trim()
            .to_string();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }
}

/// Replicated state of one entity in an area.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntitySnapshot {
    pub id: EntityId,
    pub pos: Pos,
    /// Radians, adapter space.
    pub facing: f32,
    pub anim: AnimState,
    pub avatar: AvatarAppearance,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fingerprint_roundtrip() {
        let f = Fingerprint([7u8; 32]);
        assert_eq!(Fingerprint::from_hex(&f.to_hex()).unwrap(), f);
        assert!(Fingerprint::from_hex("zz").is_err());
    }
    #[test]
    fn name_sanitize() {
        assert_eq!(AvatarAppearance::sanitize_name("  Cloud\n "), Some("Cloud".into()));
        assert_eq!(AvatarAppearance::sanitize_name(" \t"), None);
    }
}
