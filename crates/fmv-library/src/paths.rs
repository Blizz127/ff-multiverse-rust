//! State-directory resolution, logical path normalisation and set ids.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use fmv_core::{EditionProvenance, GameId, Platform};

use crate::error::LibraryError;

/// Environment variable that overrides the default state directory.
pub const STATE_DIR_ENV: &str = "FMV_STATE_DIR";
/// Longest accepted logical path (bytes) and path component (bytes).
pub const MAX_LOGICAL_PATH: usize = 1024;
pub const MAX_COMPONENT: usize = 255;
const MAX_SET_ID: usize = 160;

/// State directory: the explicit path, else `$FMV_STATE_DIR`, else `<user data dir>/ff-multiverse`.
pub fn resolve_state_dir(explicit: Option<&Path>) -> Result<PathBuf, LibraryError> {
    // an explicit path never consults the environment or the user directories
    if let Some(p) = explicit.filter(|p| !p.as_os_str().is_empty()) {
        return Ok(p.to_path_buf());
    }
    resolve_state_dir_from(None, std::env::var_os(STATE_DIR_ENV), dirs::data_dir())
}

/// Pure form of [`resolve_state_dir`]: the environment and the user data dir are injected, so
/// the precedence rules can be tested without touching process state.
pub fn resolve_state_dir_from(
    explicit: Option<&Path>,
    env_value: Option<OsString>,
    data_dir: Option<PathBuf>,
) -> Result<PathBuf, LibraryError> {
    if let Some(p) = explicit.filter(|p| !p.as_os_str().is_empty()) {
        return Ok(p.to_path_buf());
    }
    if let Some(e) = env_value.filter(|e| !e.is_empty()) {
        return Ok(PathBuf::from(e));
    }
    data_dir
        .map(|d| d.join("ff-multiverse"))
        .ok_or(LibraryError::NoStateDir)
}

/// Normalise a logical (in-set) path: `/` separated, no empty/`.` components, never absolute,
/// never containing `..`. PC paths are lowercased because Windows installs are case-insensitive;
/// disc paths keep the case found on the disc.
pub fn normalize_logical_path(raw: &str, platform: Platform) -> Result<String, LibraryError> {
    let bad = |reason: &'static str| LibraryError::BadLogicalPath {
        path: raw.chars().take(80).collect(),
        reason,
    };
    if raw.is_empty() {
        return Err(bad("empty path"));
    }
    if raw.len() > MAX_LOGICAL_PATH {
        return Err(bad("path too long"));
    }
    if raw.chars().any(char::is_control) {
        return Err(bad("control character in path"));
    }
    let unified = raw.replace('\\', "/");
    if unified.starts_with('/') {
        return Err(bad("absolute path"));
    }
    if unified.ends_with('/') {
        return Err(bad("trailing separator"));
    }
    let mut parts: Vec<&str> = Vec::new();
    for comp in unified.split('/') {
        match comp {
            "" | "." => {}
            ".." => return Err(bad("'..' component")),
            c => {
                if c.len() > MAX_COMPONENT {
                    return Err(bad("path component too long"));
                }
                parts.push(c);
            }
        }
    }
    // A leading drive designator such as `C:` makes the path absolute on Windows.
    if let Some(first) = parts.first() {
        let mut chars = first.chars();
        if let (Some(a), Some(':'), None) = (chars.next(), chars.next(), chars.next()) {
            if a.is_ascii_alphabetic() {
                return Err(bad("absolute path (drive designator)"));
            }
        }
    }
    if parts.is_empty() {
        return Err(bad("empty path"));
    }
    let joined = parts.join("/");
    Ok(if platform == Platform::Pc {
        joined.to_ascii_lowercase()
    } else {
        joined
    })
}

fn slug(s: &str) -> String {
    let mut out = String::new();
    let mut pending_dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            pending_dash = false;
            out.push(c.to_ascii_lowercase());
        } else {
            pending_dash = true;
        }
    }
    let mut out: String = out.chars().take(24).collect();
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        out.push('x');
    }
    out
}

/// The fixed text a game contributes to a set id. Written out per variant on purpose: the id is
/// persisted as a directory name, so it must not depend on `Debug` output (which is not a stable
/// interface of the frozen `fmv-core` enums). The match has no wildcard arm: a new `GameId`
/// variant is a compile error here, so its id text is chosen consciously instead of by accident.
fn game_key(g: GameId) -> &'static str {
    match g {
        GameId::Ff7 => "ff7",
        GameId::Ff8 => "ff8",
        GameId::Ff9 => "ff9",
        GameId::Ff10 => "ff10",
        GameId::Ff12 => "ff12",
        GameId::Fixture => "fixture",
    }
}

/// See [`game_key`]: the same rule for [`Platform`].
fn platform_key(p: Platform) -> &'static str {
    match p {
        Platform::Pc => "pc",
        Platform::Ps1 => "ps1",
        Platform::Synthetic => "synthetic",
        Platform::Unknown => "unknown",
    }
}

/// Stable set id for a provenance.
///
/// The readable prefix is `game-platform-edition-region-revision`. The 8-hex suffix is a blake3
/// over the exact (unslugged) strings of those five fields PLUS language and the modded flag,
/// so lossy slugs ("PC 1998" vs "pc-1998") can never collide, and a modded install or a
/// different-language install never lands in the same set as a vanilla one.
pub fn set_id_for(p: &EditionProvenance) -> String {
    let game = game_key(p.game);
    let platform = platform_key(p.platform);
    let mut h = blake3::Hasher::new();
    h.update(b"fmv-library/set-id/v1");
    for field in [
        game,
        platform,
        p.edition.as_str(),
        p.region.as_str(),
        p.language.as_str(),
        p.revision.as_str(),
    ] {
        h.update(&(field.len() as u64).to_le_bytes());
        h.update(field.as_bytes());
    }
    h.update(&[u8::from(p.modded)]);
    let hex = h.finalize().to_hex();
    let short = hex.as_str().get(..8).unwrap_or("00000000");
    format!(
        "{game}-{platform}-{}-{}-{}-{short}",
        slug(&p.edition),
        slug(&p.region),
        slug(&p.revision)
    )
}

/// Set ids are used as directory names, so only `[a-z0-9-]` is accepted.
pub fn validate_set_id(id: &str) -> Result<(), LibraryError> {
    let ok = !id.is_empty()
        && id.len() <= MAX_SET_ID
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(LibraryError::BadSetId(id.chars().take(80).collect()))
    }
}

/// 64 lowercase hex characters (a blake3 digest).
pub fn is_blake3_hex(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// One-line human description of a provenance, used in errors and listings. Fixture content
/// is labelled by [`fmv_core::GameId::label`].
pub fn describe_provenance(p: &EditionProvenance) -> String {
    format!(
        "{} / {:?} / {} / region {} / language {} / revision {}{}",
        p.game.label(),
        p.platform,
        p.edition,
        p.region,
        p.language,
        p.revision,
        if p.modded { " / MODDED" } else { "" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmv_core::GameId;

    fn prov(edition: &str, region: &str, revision: &str) -> EditionProvenance {
        EditionProvenance {
            game: GameId::Ff7,
            platform: Platform::Pc,
            edition: edition.into(),
            region: region.into(),
            language: "en".into(),
            revision: revision.into(),
            sources: vec![],
            modded: false,
        }
    }

    #[test]
    fn state_dir_precedence() {
        let data = Some(PathBuf::from("/data"));
        let env = Some(OsString::from("/from-env"));
        let explicit = PathBuf::from("/explicit");
        assert_eq!(
            resolve_state_dir_from(Some(&explicit), env.clone(), data.clone()).unwrap(),
            explicit
        );
        assert_eq!(
            resolve_state_dir_from(None, env.clone(), data.clone()).unwrap(),
            PathBuf::from("/from-env")
        );
        assert_eq!(
            resolve_state_dir_from(None, None, data.clone()).unwrap(),
            PathBuf::from("/data/ff-multiverse")
        );
        // empty values are treated as unset
        assert_eq!(
            resolve_state_dir_from(Some(Path::new("")), Some(OsString::new()), data).unwrap(),
            PathBuf::from("/data/ff-multiverse")
        );
        assert!(matches!(
            resolve_state_dir_from(None, None, None),
            Err(LibraryError::NoStateDir)
        ));
    }

    #[test]
    fn logical_paths() {
        let n = |s: &str, p| normalize_logical_path(s, p);
        assert_eq!(
            n("Data\\Field/./FLEVEL.LGP", Platform::Pc).unwrap(),
            "data/field/flevel.lgp"
        );
        // disc paths keep their case
        assert_eq!(
            n("disc1/FIELD/MD1STIN.DAT", Platform::Ps1).unwrap(),
            "disc1/FIELD/MD1STIN.DAT"
        );
        assert_eq!(n("a//b", Platform::Ps1).unwrap(), "a/b");
        for bad in [
            "",
            "/etc/passwd",
            "../x",
            "a/../b",
            "a/..",
            "..\\x",
            "a/",
            "C:/x",
            "c:\\windows",
            "a\0b",
            "a\nb",
            ".",
            "./",
        ] {
            assert!(n(bad, Platform::Pc).is_err(), "{bad:?} must be rejected");
        }
        assert!(n(&"a".repeat(MAX_LOGICAL_PATH + 1), Platform::Pc).is_err());
        assert!(n(&"a".repeat(MAX_COMPONENT + 1), Platform::Pc).is_err());
        // "C:" only counts as a drive when it is the whole first component
        assert!(n("cc:/x", Platform::Pc).is_ok());
    }

    #[test]
    fn set_ids_are_stable_distinct_and_path_safe() {
        let a = prov("PC 1998", "unknown", "unknown");
        assert_eq!(set_id_for(&a), set_id_for(&a.clone()));
        validate_set_id(&set_id_for(&a)).unwrap();
        assert!(set_id_for(&a).starts_with("ff7-pc-pc-1998-unknown-unknown-"));

        let mut seen = std::collections::HashSet::new();
        seen.insert(set_id_for(&a));
        // lossy slugs must not collide
        assert!(seen.insert(set_id_for(&prov("pc-1998", "unknown", "unknown"))));
        assert!(seen.insert(set_id_for(&prov("PC 1998", "NTSC-U", "unknown"))));
        assert!(seen.insert(set_id_for(&prov("PC 1998", "unknown", "1.1"))));
        let mut lang = a.clone();
        lang.language = "de".into();
        assert!(seen.insert(set_id_for(&lang)));
        let mut modded = a.clone();
        modded.modded = true;
        assert!(seen.insert(set_id_for(&modded)));
        let mut plat = a.clone();
        plat.platform = Platform::Ps1;
        assert!(seen.insert(set_id_for(&plat)));
        // sources do not influence identity
        let mut with_src = a.clone();
        with_src.sources.push(fmv_core::SourceRef::Fixture);
        assert_eq!(set_id_for(&with_src), set_id_for(&a));
        // hostile edition text still gives a path-safe id
        let id = set_id_for(&prov("../../etc", "a/b", "\0"));
        validate_set_id(&id).unwrap();
        assert!(validate_set_id("../x").is_err());
        assert!(validate_set_id("").is_err());
        assert!(validate_set_id("UPPER").is_err());
    }

    /// The ids are persisted as directory names (and re-validated against saved import sessions),
    /// so they must never change. These values were produced by the original `{:?}`-based
    /// implementation: they prove the move to explicit per-variant strings changed nothing.
    #[test]
    fn set_ids_are_pinned_golden_values() {
        let mk = |game, platform, edition: &str, region: &str, language: &str, revision: &str, modded| {
            set_id_for(&EditionProvenance {
                game,
                platform,
                edition: edition.into(),
                region: region.into(),
                language: language.into(),
                revision: revision.into(),
                sources: vec![],
                modded,
            })
        };
        let x = |game, platform| mk(game, platform, "x", "y", "z", "w", false);
        assert_eq!(
            mk(GameId::Ff7, Platform::Pc, "PC 1998", "unknown", "en", "unknown", false),
            "ff7-pc-pc-1998-unknown-unknown-71302c32"
        );
        assert_eq!(
            mk(GameId::Ff7, Platform::Pc, "Steam 2013", "NTSC-U", "fr", "1.02", true),
            "ff7-pc-steam-2013-ntsc-u-1-02-383635b7"
        );
        assert_eq!(
            mk(GameId::Ff7, Platform::Ps1, "PS1 original", "NTSC-U", "en", "unknown", false),
            "ff7-ps1-ps1-original-ntsc-u-unknown-d3e6983a"
        );
        assert_eq!(
            mk(GameId::Fixture, Platform::Synthetic, "fixture", "none", "none", "v1", false),
            "fixture-synthetic-fixture-none-v1-0fd83994"
        );
        assert_eq!(x(GameId::Ff8, Platform::Unknown), "ff8-unknown-x-y-w-24c1b8fa");
        assert_eq!(x(GameId::Ff9, Platform::Pc), "ff9-pc-x-y-w-63b022d9");
        assert_eq!(x(GameId::Ff10, Platform::Ps1), "ff10-ps1-x-y-w-644f20e9");
        assert_eq!(x(GameId::Ff12, Platform::Pc), "ff12-pc-x-y-w-622d8c76");
    }

    #[test]
    fn hex_check() {
        assert!(is_blake3_hex(&"ab".repeat(32)));
        assert!(!is_blake3_hex(&"AB".repeat(32)));
        assert!(!is_blake3_hex("../../etc/passwd"));
        assert!(!is_blake3_hex(&"a".repeat(63)));
    }
}
