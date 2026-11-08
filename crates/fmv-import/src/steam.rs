//! Read-only access to Steam's `appmanifest_<appid>.acf` files.
//!
//! An appmanifest is the small text file Steam keeps next to the games of a library
//! (`<library>/steamapps/appmanifest_<appid>.acf`). It names the app (`appid`), the folder the
//! app was installed into (`installdir`, below `steamapps/common/`) and the depot build
//! (`buildid`). `scan-pc` reads it, and nothing else of an install's contents, to identify a
//! Steam edition: it is Steam's own record of what was installed where, not a hash of the game
//! files, so it raises the confidence of an edition guess but never makes it verified.
//!
//! Everything here is read-only and treats the file as untrusted: the read is size-capped, the
//! parser never panics and bounds its nesting, and the one value that can end up in a label
//! (the build id) is accepted only when it is plain digits.

use std::fs;
use std::io::Read;
use std::path::Path;

use crate::scan::find_entry;

/// Steam appid of the FINAL FANTASY VII **Steam Edition** (install folder
/// `FINAL FANTASY VII Steam Edition`, game data in `ff7/workingdir/data`).
pub const STEAM_EDITION_APPID: &str = "3837340";
/// Steam appid of the classic 2013 Steam release (install folder `FINAL FANTASY VII`,
/// `ff7_en.exe`-style executables).
pub const STEAM_2013_APPID: &str = "39140";

/// Edition label of the Steam Edition. It is part of the library set id and of the area
/// fingerprint that decides which players can share an instance, so it must never change:
/// sets imported with this exact text (`--edition "Steam Edition (appid 3837340)"`) must be the
/// ones the auto-detected label reaches.
pub const STEAM_EDITION_LABEL: &str = "Steam Edition (appid 3837340)";
/// Edition label of the 2013 Steam release (pinned for the same reason).
pub const STEAM_2013_LABEL: &str = "Steam 2013";

/// The revision label for a Steam depot build: `build 21140793`. Pinned like the edition label.
pub fn build_revision(buildid: &str) -> String {
    format!("build {buildid}")
}

/// Largest appmanifest that is read (real ones are a few KB).
const MAX_ACF_BYTES: u64 = 1 << 20;
/// Deepest nesting the parser follows (real manifests nest 3 levels).
const MAX_DEPTH: usize = 16;
/// Most tokens the parser accepts.
const MAX_TOKENS: usize = 200_000;
/// Longest text of an untrusted value that is repeated in a message.
const MAX_ECHO: usize = 60;

/// What an appmanifest says about the install. Every field is validated; nothing is a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppManifest {
    /// Digits only.
    pub appid: String,
    /// The folder below `steamapps/common/` the app lives in (`FINAL FANTASY VII Steam Edition`).
    pub installdir: Option<String>,
    /// The depot build, digits only (1 to 20 of them); `None` when absent or not plain digits.
    pub buildid: Option<String>,
}

/// An appmanifest that belongs to the install being scanned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteamAppMatch {
    /// `installdir` is `Some` and names the install folder (ignoring case).
    pub manifest: AppManifest,
    /// The file name as it is on disk (`appmanifest_3837340.acf`).
    pub file_name: String,
}

/// The result of looking for the appmanifest of one appid next to an install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestLookup {
    /// The install is not `<..>/steamapps/common/<dir>`, or the file is not there.
    NotFound,
    /// A file with the right name exists but does not vouch for this folder; `reason` says why
    /// (no paths in it).
    Ignored {
        file_name: String,
        reason: String,
    },
    Matched(SteamAppMatch),
}

/// Look for `appmanifest_<appid>.acf` for the install directory `install` (a canonical path).
///
/// Steam installs an app into `<library>/steamapps/common/<installdir>` and keeps the manifest at
/// `<library>/steamapps/appmanifest_<appid>.acf`, so `install` must sit exactly there. The
/// manifest only counts when it names this appid AND this folder as its `installdir`: a manifest
/// of another app, or of another folder, says nothing about this install.
pub fn find_app_manifest(install: &Path, appid: &str) -> ManifestLookup {
    let Some(dir_name) = install
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
    else {
        return ManifestLookup::NotFound;
    };
    let Some(common) = install.parent() else {
        return ManifestLookup::NotFound;
    };
    let Some(steamapps) = common.parent() else {
        return ManifestLookup::NotFound;
    };
    let named = |p: &Path, want: &str| {
        p.file_name()
            .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(want))
    };
    if !named(common, "common") || !named(steamapps, "steamapps") {
        return ManifestLookup::NotFound;
    }
    let wanted = format!("appmanifest_{appid}.acf");
    let Some((file_name, path)) = find_entry(steamapps, &wanted) else {
        return ManifestLookup::NotFound;
    };
    let ignored = |reason: String| ManifestLookup::Ignored {
        file_name: file_name.clone(),
        reason,
    };
    let Some(manifest) = read_appmanifest(&path) else {
        return ignored("it could not be read as a Steam appmanifest".to_string());
    };
    if manifest.appid != appid {
        return ignored(format!(
            "it names appid {} instead of {appid}",
            manifest.appid
        ));
    }
    match manifest.installdir.as_deref() {
        Some(d) if d.eq_ignore_ascii_case(&dir_name) => ManifestLookup::Matched(SteamAppMatch {
            manifest,
            file_name: file_name.clone(),
        }),
        Some(d) => ignored(format!(
            "its installdir (\"{}\") is not this install folder",
            echo(d)
        )),
        None => ignored("it names no installdir".to_string()),
    }
}

/// Untrusted text made safe to repeat in a message: control characters dropped, length capped.
fn echo(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .take(MAX_ECHO)
        .collect()
}

/// Read and parse an appmanifest. `None` for anything that is not a readable, small, regular
/// file holding a well-formed `AppState` with a numeric `appid`.
pub fn read_appmanifest(path: &Path) -> Option<AppManifest> {
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_ACF_BYTES {
        return None;
    }
    let mut buf = Vec::new();
    fs::File::open(path)
        .ok()?
        .take(MAX_ACF_BYTES + 1)
        .read_to_end(&mut buf)
        .ok()?;
    if buf.len() as u64 > MAX_ACF_BYTES {
        return None;
    }
    parse_appmanifest(&String::from_utf8_lossy(&buf))
}

/// Parse the text of an appmanifest (Valve KeyValues). `None` when it is malformed, has no
/// `AppState`, or its `appid` is not 1 to 12 digits.
pub fn parse_appmanifest(text: &str) -> Option<AppManifest> {
    let tokens = tokenize(text)?;
    let top = parse_items(&mut tokens.into_iter(), 0)?;
    let Kv::Obj(state) = find(&top, "AppState")? else {
        return None;
    };
    let appid = find_str(state, "appid")?.trim().to_string();
    if !(1..=12).contains(&appid.len()) || !appid.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let installdir = find_str(state, "installdir")
        .map(str::trim)
        .filter(|d| !d.is_empty() && d.chars().all(|c| !c.is_control()))
        .map(str::to_string);
    let buildid = find_str(state, "buildid")
        .map(str::trim)
        .filter(|b| (1..=20).contains(&b.len()) && b.bytes().all(|c| c.is_ascii_digit()))
        .map(str::to_string);
    Some(AppManifest {
        appid,
        installdir,
        buildid,
    })
}

/// A parsed KeyValues value.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Kv {
    Str(String),
    Obj(Vec<(String, Kv)>),
}

/// First entry named `key` (ASCII case-insensitive, as Steam reads keys).
fn find<'a>(items: &'a [(String, Kv)], key: &str) -> Option<&'a Kv> {
    items
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v)
}

fn find_str<'a>(items: &'a [(String, Kv)], key: &str) -> Option<&'a str> {
    match find(items, key)? {
        Kv::Str(s) => Some(s.as_str()),
        Kv::Obj(_) => None,
    }
}

enum Tok {
    Str(String),
    Open,
    Close,
}

/// Split KeyValues text into strings and braces. Quoted strings take the usual `\n \t \\ \"`
/// escapes; `//` starts a comment; a bare word is a string too. `None` for an unterminated
/// string or an absurd number of tokens.
fn tokenize(text: &str) -> Option<Vec<Tok>> {
    let mut out = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(&c) = chars.peek() {
        if out.len() > MAX_TOKENS {
            return None;
        }
        match c {
            c if c.is_whitespace() || c == '\u{feff}' => {
                chars.next();
            }
            '{' => {
                chars.next();
                out.push(Tok::Open);
            }
            '}' => {
                chars.next();
                out.push(Tok::Close);
            }
            '"' => {
                chars.next();
                let mut s = String::new();
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => match chars.next()? {
                            'n' => s.push('\n'),
                            't' => s.push('\t'),
                            '\\' => s.push('\\'),
                            '"' => s.push('"'),
                            other => {
                                s.push('\\');
                                s.push(other);
                            }
                        },
                        ch => s.push(ch),
                    }
                }
                out.push(Tok::Str(s));
            }
            '/' => {
                chars.next();
                if chars.peek() == Some(&'/') {
                    // a comment runs to the end of the line
                    for ch in chars.by_ref() {
                        if ch == '\n' {
                            break;
                        }
                    }
                } else {
                    let mut s = String::from('/');
                    bare_word(&mut chars, &mut s);
                    out.push(Tok::Str(s));
                }
            }
            _ => {
                let mut s = String::new();
                bare_word(&mut chars, &mut s);
                out.push(Tok::Str(s));
            }
        }
    }
    Some(out)
}

/// Append the rest of an unquoted word (up to whitespace, a brace or a quote) to `s`.
fn bare_word(chars: &mut std::iter::Peekable<std::str::Chars<'_>>, s: &mut String) {
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() || matches!(c, '{' | '}' | '"') {
            break;
        }
        s.push(c);
        chars.next();
    }
}

/// Parse `key value` / `key { ... }` entries up to the closing brace (or the end of the text at
/// the top level). `None` for anything malformed: a half-written manifest is not trusted.
fn parse_items(toks: &mut std::vec::IntoIter<Tok>, depth: usize) -> Option<Vec<(String, Kv)>> {
    if depth > MAX_DEPTH {
        return None;
    }
    let mut items = Vec::new();
    loop {
        match toks.next() {
            None => return (depth == 0).then_some(items),
            Some(Tok::Close) => return (depth > 0).then_some(items),
            Some(Tok::Open) => return None,
            Some(Tok::Str(key)) => match toks.next()? {
                Tok::Str(value) => items.push((key, Kv::Str(value))),
                Tok::Open => items.push((key, Kv::Obj(parse_items(toks, depth + 1)?))),
                Tok::Close => return None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synthetic::steam_appmanifest;

    #[test]
    fn a_real_looking_manifest_is_read() {
        let m = parse_appmanifest(&steam_appmanifest(
            "3837340",
            "FINAL FANTASY VII Steam Edition",
            "21140793",
        ))
        .unwrap();
        assert_eq!(m.appid, "3837340");
        assert_eq!(
            m.installdir.as_deref(),
            Some("FINAL FANTASY VII Steam Edition")
        );
        assert_eq!(m.buildid.as_deref(), Some("21140793"));
    }

    #[test]
    fn the_labels_are_pinned() {
        // These strings are part of persisted set ids and area fingerprints: they never change.
        assert_eq!(STEAM_EDITION_LABEL, "Steam Edition (appid 3837340)");
        assert_eq!(STEAM_2013_LABEL, "Steam 2013");
        assert_eq!(build_revision("21140793"), "build 21140793");
        assert!(STEAM_EDITION_LABEL.contains(STEAM_EDITION_APPID));
    }

    #[test]
    fn comments_escapes_case_bare_words_and_a_bom_are_tolerated() {
        let text = "\u{feff}// leading comment\n\"appstate\" // trailing\n{\n  APPID 39140\n  \"InstallDir\" \"A \\\"quoted\\\" dir\"\n  \"BuildID\" \"7\"\n}\n";
        let m = parse_appmanifest(text).unwrap();
        assert_eq!(m.appid, "39140");
        assert_eq!(m.installdir.as_deref(), Some("A \"quoted\" dir"));
        assert_eq!(m.buildid.as_deref(), Some("7"));
    }

    #[test]
    fn values_that_could_end_up_in_a_label_are_validated() {
        // a build id that is not plain digits is dropped, not trusted
        for bad in ["", "12 34", "12a", "-5", "１２", "123456789012345678901"] {
            let m = parse_appmanifest(&steam_appmanifest("3837340", "x", bad)).unwrap();
            assert_eq!(m.buildid, None, "{bad:?}");
        }
        // the appid must be digits, and short
        for bad in ["", "abc", "12 3", "1234567890123"] {
            assert!(
                parse_appmanifest(&steam_appmanifest(bad, "x", "1")).is_none(),
                "{bad:?}"
            );
        }
        // an installdir with control characters is not used
        let m = parse_appmanifest(&steam_appmanifest("3837340", "a\\nb", "1")).unwrap();
        assert_eq!(m.installdir, None);
    }

    #[test]
    fn malformed_text_is_refused_without_panicking() {
        for bad in [
            "",
            "{",
            "}",
            "\"AppState\"",
            "\"AppState\" {",
            "\"AppState\" { \"appid\" }",
            "\"AppState\" { \"appid\" \"1\" ",
            "\"AppState\" { \"appid\" \"1",
            "\"AppState\" } ",
            "\"AppState\" \"x\"",
            "\"Other\" { \"appid\" \"1\" }",
            "\"AppState\" { \"appid\" \"\" }",
            "\"AppState\" { { } }",
            "\"a\\",
        ] {
            assert!(parse_appmanifest(bad).is_none(), "{bad:?}");
        }
        // nesting beyond the bound is refused rather than recursed into
        let deep = format!(
            "{}{}",
            "\"k\" {".repeat(MAX_DEPTH + 4),
            "}".repeat(MAX_DEPTH + 4)
        );
        assert!(parse_appmanifest(&deep).is_none());
        // junk bytes (lossy-decoded) never panic
        let junk: Vec<u8> = (0..4096u32)
            .map(|i| (i.wrapping_mul(2654435761) >> 13) as u8)
            .collect();
        let _ = parse_appmanifest(&String::from_utf8_lossy(&junk));
    }

    #[test]
    fn lookup_needs_the_steamapps_layout_the_appid_and_the_folder_name() {
        let tmp = tempfile::tempdir().unwrap();
        let apps = tmp.path().join("steamapps");
        let install = apps.join("common").join("FINAL FANTASY VII Steam Edition");
        fs::create_dir_all(&install).unwrap();
        let write = |name: &str, text: &str| fs::write(apps.join(name), text).unwrap();

        // no file: nothing
        assert_eq!(
            find_app_manifest(&install, STEAM_EDITION_APPID),
            ManifestLookup::NotFound
        );
        // a matching manifest (the file name and the installdir are matched ignoring case)
        write(
            "APPMANIFEST_3837340.ACF",
            &steam_appmanifest("3837340", "final fantasy vii steam edition", "21140793"),
        );
        match find_app_manifest(&install, STEAM_EDITION_APPID) {
            ManifestLookup::Matched(m) => {
                assert_eq!(m.file_name, "APPMANIFEST_3837340.ACF");
                assert_eq!(m.manifest.buildid.as_deref(), Some("21140793"));
            }
            other => panic!("{other:?}"),
        }
        // the manifest of another appid is another lookup, and is not found here
        assert_eq!(
            find_app_manifest(&install, STEAM_2013_APPID),
            ManifestLookup::NotFound
        );
        // (Linux file names are case sensitive: remove the upper-case one before the next cases)
        fs::remove_file(apps.join("APPMANIFEST_3837340.ACF")).unwrap();

        // a manifest that names another appid, another folder, no folder, or is junk: ignored,
        // with a reason, and never trusted
        for (text, why) in [
            (
                steam_appmanifest("39140", "FINAL FANTASY VII Steam Edition", "5"),
                "appid 39140",
            ),
            (
                steam_appmanifest("3837340", "FINAL FANTASY VII", "5"),
                "installdir",
            ),
            (
                "\"AppState\" { \"appid\" \"3837340\" }".to_string(),
                "no installdir",
            ),
            ("not a manifest".to_string(), "could not be read"),
        ] {
            write("appmanifest_3837340.acf", &text);
            match find_app_manifest(&install, STEAM_EDITION_APPID) {
                ManifestLookup::Ignored { reason, .. } => {
                    assert!(reason.contains(why), "{reason}");
                    assert!(!reason.contains(&tmp.path().display().to_string()));
                }
                other => panic!("{text}: {other:?}"),
            }
        }

        // an install that is not <steamapps>/common/<dir> never finds one
        let elsewhere = tmp
            .path()
            .join("games")
            .join("FINAL FANTASY VII Steam Edition");
        fs::create_dir_all(&elsewhere).unwrap();
        assert_eq!(
            find_app_manifest(&elsewhere, STEAM_EDITION_APPID),
            ManifestLookup::NotFound
        );
        assert_eq!(
            find_app_manifest(Path::new("/"), STEAM_EDITION_APPID),
            ManifestLookup::NotFound
        );
    }

    #[test]
    fn oversized_or_non_regular_manifests_are_not_read() {
        let tmp = tempfile::tempdir().unwrap();
        let big = tmp.path().join("big.acf");
        let mut text = steam_appmanifest("3837340", "x", "1");
        text.push_str(&" ".repeat(MAX_ACF_BYTES as usize));
        fs::write(&big, text).unwrap();
        assert!(read_appmanifest(&big).is_none());
        assert!(
            read_appmanifest(tmp.path()).is_none(),
            "a directory is not a manifest"
        );
        assert!(read_appmanifest(&tmp.path().join("missing.acf")).is_none());
    }
}
