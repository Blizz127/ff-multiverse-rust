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
    /// Every game, in declaration order.
    pub const ALL: [GameId; 6] = [GameId::Ff7, GameId::Ff8, GameId::Ff9, GameId::Ff10, GameId::Ff12, GameId::Fixture];

    /// Stable machine-readable name: exactly the serde variant name (`"Ff7"`, `"Fixture"`, ...),
    /// which is also what `fmv-persist` uses as a file name and map key. It is safe in file
    /// names and log lines (ASCII letters and digits), and [`GameId::from_str`] is its inverse.
    pub fn key(self) -> &'static str {
        match self {
            GameId::Ff7 => "Ff7",
            GameId::Ff8 => "Ff8",
            GameId::Ff9 => "Ff9",
            GameId::Ff10 => "Ff10",
            GameId::Ff12 => "Ff12",
            GameId::Fixture => "Fixture",
        }
    }

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

/// `s` is not the [`GameId::key`] of any game.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseGameIdError;

impl fmt::Display for ParseGameIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "not a game key (expected one of Ff7, Ff8, Ff9, Ff10, Ff12, Fixture)")
    }
}

impl std::error::Error for ParseGameIdError {}

impl std::str::FromStr for GameId {
    type Err = ParseGameIdError;

    /// Exact, case-sensitive inverse of [`GameId::key`].
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        GameId::ALL.into_iter().find(|g| g.key() == s).ok_or(ParseGameIdError)
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

    /// Build an appearance. Prefer this to a struct literal: new optional fields can then be
    /// added without breaking callers.
    pub fn new(display_name: impl Into<String>, tint: [u8; 3], appearance_ref: Option<String>) -> Self {
        Self { display_name: display_name.into(), tint, appearance_ref }
    }

    /// Clean a player-supplied display name: drop control characters and invisible formatting
    /// characters (see [`is_invisible_format`]), clamp to [`Self::MAX_NAME`] characters, trim
    /// surrounding whitespace. Returns `None` if nothing visible is left.
    ///
    /// Names are shown to every player and used to tell players apart, so a name that looks
    /// like another one but carries zero-width or bidi-override characters must not survive.
    pub fn sanitize_name(raw: &str) -> Option<String> {
        let s: String = raw
            .chars()
            .filter(|c| !c.is_control() && !is_invisible_format(*c))
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

/// Characters that draw nothing, or change how the text around them is drawn or ordered, so
/// that two different strings can look identical (Unicode category Cf, plus the line and
/// paragraph separators U+2028/U+2029 that split a line for many log parsers). This is the
/// list of format characters of Unicode 16, written out because `std` has no category query.
pub fn is_invisible_format(c: char) -> bool {
    matches!(
        u32::from(c),
        0x00AD                       // soft hyphen
        | 0x0600..=0x0605            // Arabic number signs
        | 0x061C                     // Arabic letter mark
        | 0x06DD                     // Arabic end of ayah
        | 0x070F                     // Syriac abbreviation mark
        | 0x0890..=0x0891            // Arabic pound / piastre marks above
        | 0x08E2                     // Arabic disputed end of ayah
        | 0x180E                     // Mongolian vowel separator
        | 0x200B..=0x200F            // zero-width space/joiners, LRM, RLM
        | 0x2028..=0x2029            // line and paragraph separators
        | 0x202A..=0x202E            // bidi embeddings and overrides
        | 0x2060..=0x2064            // word joiner, invisible operators
        | 0x2066..=0x206F            // bidi isolates, deprecated format characters
        | 0xFEFF                     // zero-width no-break space (BOM)
        | 0xFFF9..=0xFFFB            // interlinear annotation
        | 0x110BD | 0x110CD          // Kaithi number signs
        | 0x13430..=0x1343F          // Egyptian hieroglyph format controls
        | 0x1BCA0..=0x1BCA3          // shorthand format controls
        | 0x1D173..=0x1D17A          // musical symbol format controls
        | 0xE0001                    // language tag
        | 0xE0020..=0xE007F          // tag characters
    )
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

    #[test]
    fn names_lose_invisible_formatting_characters() {
        // Zero-width characters cannot make "Cloud" look like "Cloud" while being another string.
        let zw = "Cl\u{200B}ou\u{200D}d\u{FEFF}";
        assert_eq!(AvatarAppearance::sanitize_name(zw), Some("Cloud".into()));
        // Bidi overrides and isolates, the soft hyphen, the word joiner and tag characters.
        for ch in ['\u{202E}', '\u{202D}', '\u{2066}', '\u{2069}', '\u{200E}', '\u{061C}', '\u{00AD}', '\u{2060}', '\u{E0041}'] {
            let n = format!("A{ch}B");
            assert_eq!(AvatarAppearance::sanitize_name(&n), Some("AB".into()), "U+{:04X}", u32::from(ch));
        }
        // Line and paragraph separators.
        assert_eq!(AvatarAppearance::sanitize_name("a\u{2028}b\u{2029}c"), Some("abc".into()));
        // A name of nothing visible is no name.
        assert_eq!(AvatarAppearance::sanitize_name("\u{200B}\u{202E}\u{FEFF}"), None);
        assert_eq!(AvatarAppearance::sanitize_name(" \u{200B} \u{3000}"), None);
        // The invisible characters are removed BEFORE the length clamp, so they cannot eat the
        // visible budget and two names that differ only in them clamp to the same string.
        let padded: String = std::iter::repeat('\u{200B}').take(40).chain("x".repeat(30).chars()).collect();
        let clean = AvatarAppearance::sanitize_name(&padded).unwrap();
        assert_eq!(clean.chars().count(), AvatarAppearance::MAX_NAME);
        assert!(clean.chars().all(|c| c == 'x'));
        // Ordinary text, including non-ASCII letters, emoji and ZWJ-free scripts, is untouched.
        for ok in ["Cloud", "Red XIII", "Ärger", "\u{30af}\u{30e9}\u{30a6}\u{30c9}", "Aerith \u{1F338}", "O'Neil-2"] {
            assert_eq!(AvatarAppearance::sanitize_name(ok), Some(ok.into()), "{ok}");
        }
        // Idempotent: cleaning twice changes nothing more.
        let once = AvatarAppearance::sanitize_name(zw).unwrap();
        assert_eq!(AvatarAppearance::sanitize_name(&once), Some(once));
    }

    #[test]
    fn game_keys_are_the_serde_names_and_round_trip() {
        use std::str::FromStr;
        for g in GameId::ALL {
            // The same strings fmv-persist uses on disk, so the two can never drift.
            assert_eq!(serde_json::to_value(g).unwrap(), serde_json::Value::String(g.key().to_string()));
            assert_eq!(GameId::from_str(g.key()), Ok(g));
            assert!(g.key().bytes().all(|b| b.is_ascii_alphanumeric()));
        }
        assert_eq!(GameId::ALL.len(), 6);
        for bad in ["ff7", "FF7", "Ff 7", "", "Fixture\n", "../Ff7", "Ff13"] {
            assert_eq!(GameId::from_str(bad), Err(ParseGameIdError), "{bad:?}");
        }
        assert!(ParseGameIdError.to_string().contains("Ff7"));
    }

    #[test]
    fn appearance_constructor_matches_the_literal() {
        let a = AvatarAppearance::new("Tifa", [1, 2, 3], Some("model".into()));
        assert_eq!(a, AvatarAppearance { display_name: "Tifa".into(), tint: [1, 2, 3], appearance_ref: Some("model".into()) });
    }
}
