//! Game-agnostic core types shared by server, client, library and every game adapter.
//!
//! Rule: this crate never depends on an adapter. Nothing here may hold game asset bytes;
//! assets are referenced by id + [`Fingerprint`] only.

mod names;

pub use names::{is_invisible_format, name_key, NameError, MAX_MARKS};

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
    /// names and log lines (ASCII letters and digits), and `GameId::from_str` (the
    /// [`std::str::FromStr`] impl) is its inverse.
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
    /// Longest display name, in characters.
    pub const MAX_NAME: usize = 24;

    /// Longest `appearance_ref`, in characters. Together with the display name this bounds every
    /// entity snapshot, and therefore the size of an `AreaSnapshot` frame. The server and the
    /// account store (`fmv-persist`) both enforce exactly this limit (characters, not bytes:
    /// 64 four-byte characters are fine).
    pub const MAX_APPEARANCE_REF: usize = 64;

    /// Build an appearance. Prefer this to a struct literal: new optional fields can then be
    /// added without breaking callers.
    pub fn new(display_name: impl Into<String>, tint: [u8; 3], appearance_ref: Option<String>) -> Self {
        Self { display_name: display_name.into(), tint, appearance_ref }
    }

    /// Clean a player-supplied display name. Returns `None` if nothing visible is left.
    ///
    /// Removes control characters and everything that draws nothing or only steers the layout
    /// ([`is_invisible_format`]: zero-width and bidi characters, the Hangul, Braille and other
    /// invisible fillers, variation selectors, tag characters ...), writes the rest in NFC,
    /// turns every kind of white space into one plain space, collapses runs of spaces and trims
    /// them, keeps combining marks attached to a letter with at most [`MAX_MARKS`] stacked, and
    /// clamps to [`Self::MAX_NAME`] characters. The cleaning is idempotent.
    ///
    /// Names are shown to every player and used to tell players apart, so a name that looks
    /// like another one but carries invisible characters must not survive. Note what this does
    /// **not** do: it never judges the writing system (so it can safely filter names that were
    /// registered long ago), and two names it leaves different can still look alike. Use
    /// [`Self::validate_new_name`] to admit a name and [`name_key`] to compare names.
    pub fn sanitize_name(raw: &str) -> Option<String> {
        names::clean_name(raw)
    }

    /// The admission rule for a name that is about to be registered: [`Self::sanitize_name`],
    /// then the script rule (single script, or the usual East Asian combinations with Latin;
    /// see the module documentation of the name policy). `Ok` carries the cleaned name.
    ///
    /// `Latin` mixed with `Cyrillic` or `Greek` is the classic impersonation ("Tifa" with a
    /// Cyrillic T); it is refused with [`NameError::MixedScripts`].
    pub fn validate_new_name(raw: &str) -> Result<String, NameError> {
        let name = names::clean_name(raw).ok_or(NameError::Empty)?;
        names::check_scripts(&name)?;
        Ok(name)
    }

    /// Is `value` acceptable as a per-game `appearance_ref`: not empty, at most
    /// [`Self::MAX_APPEARANCE_REF`] characters, no control characters. Exactly what
    /// [`Self::clean_appearance_ref`] can produce, and nothing stricter.
    pub fn is_valid_appearance_ref(value: &str) -> bool {
        !value.is_empty()
            && value.chars().count() <= Self::MAX_APPEARANCE_REF
            && !value.chars().any(char::is_control)
    }

    /// Clean a client-supplied `appearance_ref`: drop control characters, clamp to
    /// [`Self::MAX_APPEARANCE_REF`] characters, trim; `None` if nothing is left. The result
    /// always satisfies [`Self::is_valid_appearance_ref`].
    pub fn clean_appearance_ref(raw: &str) -> Option<String> {
        let cleaned: String = raw.chars().filter(|c| !c.is_control()).take(Self::MAX_APPEARANCE_REF).collect();
        let cleaned = cleaned.trim();
        (!cleaned.is_empty()).then(|| cleaned.to_string())
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
        let padded: String = std::iter::repeat_n('\u{200B}', 40).chain("x".repeat(30).chars()).collect();
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

    #[test]
    fn appearance_ref_limits_count_characters_not_bytes() {
        // 64 four-byte characters are 256 bytes and still valid: the limit is in characters, the
        // same unit the server has always used (a byte limit is what burned names before).
        let wide = "\u{1F600}".repeat(AvatarAppearance::MAX_APPEARANCE_REF);
        assert_eq!(wide.len(), 256);
        assert!(AvatarAppearance::is_valid_appearance_ref(&wide));
        assert!(!AvatarAppearance::is_valid_appearance_ref(&format!("{wide}x")));
        // 43 CJK characters are 129 bytes: over the old store limit, fine now.
        assert!(AvatarAppearance::is_valid_appearance_ref(&"\u{6F22}".repeat(43)));
        assert!(AvatarAppearance::is_valid_appearance_ref("cloud_field_model"));
        for bad in ["", "a\nb", "a\u{0}b", "\u{1b}[31m", &"x".repeat(65)] {
            assert!(!AvatarAppearance::is_valid_appearance_ref(bad), "{bad:?}");
        }
    }

    #[test]
    fn cleaned_appearance_refs_are_always_valid() {
        let long = "m".repeat(10_000);
        let cleaned = AvatarAppearance::clean_appearance_ref(&format!("\u{1b}[31m{long}")).unwrap();
        assert_eq!(cleaned.chars().count(), AvatarAppearance::MAX_APPEARANCE_REF);
        assert_eq!(AvatarAppearance::clean_appearance_ref(" \n"), None);
        assert_eq!(AvatarAppearance::clean_appearance_ref(""), None);
        assert_eq!(AvatarAppearance::clean_appearance_ref(" model ").as_deref(), Some("model"));
        for raw in [long.as_str(), " model ", "a\u{0}b", "\u{1F600}\u{1F600}", "  x"] {
            if let Some(cleaned) = AvatarAppearance::clean_appearance_ref(raw) {
                assert!(AvatarAppearance::is_valid_appearance_ref(&cleaned), "{raw:?}");
            }
        }
        let wide = "\u{1F600}".repeat(200);
        let cleaned = AvatarAppearance::clean_appearance_ref(&wide).unwrap();
        assert_eq!(cleaned.chars().count(), AvatarAppearance::MAX_APPEARANCE_REF);
        assert!(AvatarAppearance::is_valid_appearance_ref(&cleaned));
    }

    #[test]
    fn the_previous_invisible_list_is_still_covered() {
        // Everything the old Cf-based list removed is still removed, so no stored name or test
        // that relied on it changes meaning.
        for cp in [
            0x00ADu32, 0x0600, 0x0605, 0x061C, 0x06DD, 0x070F, 0x0890, 0x0891, 0x08E2, 0x180E, 0x200B, 0x200F, 0x2028, 0x2029, 0x202A,
            0x202E, 0x2060, 0x2064, 0x2066, 0x206F, 0xFEFF, 0xFFF9, 0xFFFB, 0x110BD, 0x110CD, 0x13430, 0x1343F, 0x1BCA0, 0x1BCA3, 0x1D173,
            0x1D17A, 0xE0001, 0xE0020, 0xE007F,
        ] {
            assert!(is_invisible_format(char::from_u32(cp).unwrap()), "U+{cp:04X}");
        }
    }
}
