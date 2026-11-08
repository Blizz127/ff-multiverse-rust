//! Read-only scan of an FF7 PC install.
//!
//! The scan lists directories and reads file metadata (names, sizes). It never opens a game
//! file's contents and never writes anything, anywhere. The one file it does read is Steam's own
//! `appmanifest_<appid>.acf` text file (a few KB, see [`crate::steam`]) when the install sits in
//! a Steam library: it names the edition and the build. Edition and language guesses come from
//! file presence and that record alone and are UNVERIFIED (no game file is hashed or compared
//! with a known release); the layout under `data/lang-*` is uncertain, so every location found
//! is reported and none is assumed to be canonical.
//!
//! Where the data is: the scanned directory is either the folder that holds `data/` (the classic
//! layouts, or the Steam Edition's `ff7/workingdir`) or the install root of the Steam Edition
//! (`.../FINAL FANTASY VII Steam Edition`, data in `ff7/workingdir/data`), or its `ff7` folder.
//! The scan finds the folder that holds `data/` (the "game directory"); file paths in the report
//! are relative to the scanned directory, and the library paths of the files (`data/...`) are
//! relative to the game directory, so every accepted spelling of one install imports the same
//! set.
//!
//! Symbolic links: a link INSIDE the scanned directory is followed (a layout that links
//! `data/field/flevel.lgp` to a file elsewhere in the install is fine), a link that leads OUT of
//! it is not. A candidate file whose real path (every link resolved) is not under the scanned
//! directory is left out of the report as if it were not there, and listed in
//! [`ScanReport::refused_links`] with a warning, so that an install tree cannot make an import
//! copy some other file of the machine into the library under the name of a game file.
//! `import-pc` applies the same rule again, to the real files, right before it reads them.
//!
//! One library set holds ONE language. The scan reports every candidate file tagged with its
//! language (`None` for the language-neutral classic layout and `ff7.exe`), and
//! [`ScanReport::select`] picks "the neutral files plus exactly one language": an install that
//! holds several languages must say which one it means, so files of different languages are
//! never mixed under one language label.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use fmv_core::Platform;
use fmv_library::normalize_logical_path;
use serde::Serialize;

use crate::steam::{
    build_revision, find_app_manifest, ManifestLookup, SteamAppMatch, STEAM_2013_APPID,
    STEAM_2013_LABEL, STEAM_EDITION_APPID, STEAM_EDITION_LABEL,
};

/// 2: the report names the game directory (`data_dir`), field archives carry their language and
/// library path, and the edition guess can be `high` and carries Steam appmanifest evidence.
pub const REPORT_VERSION: u32 = 2;

/// Start of the warning that says the language was defaulted to `en` (see
/// [`ScanReport::language_defaulted`]). `import-pc` recognises it by this prefix.
pub const LANGUAGE_DEFAULT_PREFIX: &str = "language defaulted to";

struct Wanted {
    name: &'static str,
    /// Classic layout, relative to the game directory (the folder that holds `data/`), `/`
    /// separated.
    rel: &'static str,
    milestone: &'static str,
    /// Per-language variants of this file that sit next to it in the classic layout.
    variants: &'static [Variant],
}

/// A language variant of a [`Wanted`] file: a file next to it, with another name, that holds the
/// same thing for one language.
struct Variant {
    file: &'static str,
    language: &'static str,
}

/// The Steam Edition keeps one field archive per language in `data/field/`: `flevel.lgp` and the
/// variants below. The languages are INFERRED from the names alone (f = French, g = German,
/// s = Spanish, j = Japanese) and `flevel.lgp` is taken as the English one [verify].
const FIELD_VARIANTS: &[Variant] = &[
    Variant {
        file: "fflevel.lgp",
        language: "fr",
    },
    Variant {
        file: "gflevel.lgp",
        language: "de",
    },
    Variant {
        file: "sflevel.lgp",
        language: "es",
    },
    Variant {
        file: "jfleve.lgp",
        language: "ja",
    },
];

/// The language the classic file (`flevel.lgp`) takes when language variants of it exist.
const CLASSIC_VARIANT_LANGUAGE: &str = "en";

const WANTED: &[Wanted] = &[
    Wanted {
        name: "flevel.lgp",
        rel: "data/field/flevel.lgp",
        milestone: "M1",
        variants: FIELD_VARIANTS,
    },
    Wanted {
        name: "char.lgp",
        rel: "data/field/char.lgp",
        milestone: "M1",
        variants: &[],
    },
    Wanted {
        name: "battle.lgp",
        rel: "data/battle/battle.lgp",
        milestone: "M2",
        variants: &[],
    },
    Wanted {
        name: "scene.bin",
        rel: "data/battle/scene.bin",
        milestone: "M2",
        variants: &[],
    },
    Wanted {
        name: "kernel.bin",
        rel: "data/kernel/kernel.bin",
        milestone: "M2",
        variants: &[],
    },
    Wanted {
        name: "kernel2.bin",
        rel: "data/kernel/kernel2.bin",
        milestone: "M2",
        variants: &[],
    },
];

#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error("{0} is not a directory")]
    NotADirectory(PathBuf),
    #[error("cannot read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct FoundFile {
    /// Relative to the scanned directory, with the case found on disk.
    pub relative_path: String,
    pub size: u64,
    /// `classic` (`data/...`) or the language directory name (`lang-en`).
    pub layout: String,
    /// The language this file belongs to (`None`: language-neutral): the language directory's, or,
    /// for the per-language field archives of the Steam Edition, the one the file name says.
    pub language: Option<String>,
    /// True when `language` was inferred from file names ([verify]) instead of read from a
    /// language directory.
    pub language_inferred: bool,
    /// Where the file lands in a library set: relative to the game directory (the folder that
    /// holds `data/`), lower case. `None` when the name cannot be stored.
    pub logical_path: Option<String>,
    /// `Some("flevel.lgp")` when this is a language variant of that file (`fflevel.lgp`), stored
    /// under its name in `data/lang-<xx>/field/` so that readers find it as that language's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variant_of: Option<&'static str>,
}

impl FoundFile {
    /// A neutral, plainly named file `rel_in_game` below the game directory.
    fn at(game: &GameDir, rel_in_game: &str, size: u64, layout: &str, logical: &str) -> FoundFile {
        FoundFile {
            relative_path: game.join(rel_in_game),
            size,
            layout: layout.to_string(),
            language: None,
            language_inferred: false,
            logical_path: normalize_logical_path(logical, Platform::Pc).ok(),
            variant_of: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RequiredFile {
    pub name: &'static str,
    pub milestone: &'static str,
    pub found: bool,
    pub locations: Vec<FoundFile>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Executable {
    pub relative_path: String,
    pub size: u64,
    pub language: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModMarker {
    pub kind: &'static str,
    pub evidence: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct EditionGuess {
    pub edition: String,
    /// `none`, `low`, `medium` or `high`. `high` needs Steam's own appmanifest for this install
    /// (read, and agreeing with the files); it still does not mean verified: no game file is
    /// compared with a known release.
    pub confidence: &'static str,
    /// What the guess rests on, one sentence each. Never holds a local path.
    pub basis: Vec<String>,
    /// Always false: the guess is not checked against hashes of known releases.
    pub verified: bool,
    /// The revision label `import-pc` uses when `--revision` is not given: `build <id>` from the
    /// Steam appmanifest of the Steam Edition. `None`: the revision is unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    /// What Steam's appmanifest said, when one for this install was found and used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub steam: Option<SteamEvidence>,
}

/// What a Steam appmanifest that belongs to the scanned install says (see [`crate::steam`]).
#[derive(Debug, Clone, Serialize)]
pub struct SteamEvidence {
    pub appid: String,
    /// The depot build (digits), when the manifest has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub buildid: Option<String>,
    /// The folder the manifest says the app lives in (it is the install folder's name).
    pub installdir: String,
    /// The manifest's file name (`appmanifest_3837340.acf`); its location is not reported.
    pub manifest_file: String,
}

impl SteamEvidence {
    fn from_match(m: &SteamAppMatch) -> SteamEvidence {
        SteamEvidence {
            appid: m.manifest.appid.clone(),
            buildid: m.manifest.buildid.clone(),
            installdir: m.manifest.installdir.clone().unwrap_or_default(),
            manifest_file: m.file_name.clone(),
        }
    }
}

/// A file that would have been a candidate but is a symbolic link leading out of the scanned
/// directory (see the module docs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RefusedLink {
    /// Relative to the scanned directory.
    pub relative_path: String,
    pub reason: &'static str,
}

const LINK_REFUSED: &str = "symbolic link that leads outside the scanned directory (not followed)";

#[derive(Debug, Clone, Serialize)]
pub struct Milestone {
    pub ready: bool,
    pub missing: Vec<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WouldImport {
    pub logical_path: String,
    pub source_relative_path: String,
    pub size: u64,
    /// `m1`, `m2` or `identify` (executables, kept so the edition can be checked by hash later).
    pub role: &'static str,
    /// The language this file belongs to: `Some("en")` for `data/lang-en/...` and `ff7_en.exe`,
    /// `None` for language-neutral files (the classic `data/...` layout and `ff7.exe`).
    pub language: Option<String>,
    /// True when `language` was inferred from file names ([verify]) instead of read from a
    /// language directory or executable name (the per-language field archives of the Steam
    /// Edition).
    pub language_inferred: bool,
    /// `Some("flevel.lgp")` when the file is a language variant of that file (`fflevel.lgp`) and
    /// is imported under its name, in `data/lang-<xx>/field/`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variant_of: Option<&'static str>,
    /// True when `import-pc` WITHOUT `--language` would copy this file: the language-neutral
    /// files plus the language the install implies. All false when the install holds several
    /// languages and none is implied (`language_selection_required`): the import then needs
    /// `--language`, and takes that language's files plus the neutral ones.
    pub in_default_import: bool,
}

/// A language the install holds, with what says so.
#[derive(Debug, Clone, Serialize)]
pub struct LanguageOption {
    /// Lower-case code taken from the name (`en` from `ff7_en.exe` / `data/lang-en`).
    pub language: String,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LanguageError {
    #[error(
        "this install holds several languages ({available}) and a library set holds exactly one: \
         pass --language <code> to choose which one to import (run again for another)"
    )]
    Ambiguous { available: String },
    #[error("--language {given} matches nothing in this install (it holds: {available})")]
    NotInInstall { given: String, available: String },
}

/// What an import takes from a scanned install (see [`ScanReport::select`]).
#[derive(Debug)]
pub struct Selection<'a> {
    /// The language label of the set (`unknown` when the install has no language markers).
    pub language: String,
    /// The files to import: the language-neutral ones plus the chosen language's own.
    pub files: Vec<&'a WouldImport>,
    /// Other languages the install holds that are NOT part of this import.
    pub left_out: Vec<String>,
}

impl Selection<'_> {
    /// True when the selection holds a `flevel.lgp` (the field archive, required for M1).
    pub fn has_flevel(&self) -> bool {
        self.files
            .iter()
            .any(|w| w.logical_path.rsplit('/').next() == Some("flevel.lgp"))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ScanReport {
    pub tool: &'static str,
    pub report_version: u32,
    pub root: String,
    pub read_only: bool,
    /// The `data` directory the scan used, relative to the scanned directory (`data`, or
    /// `ff7/workingdir/data` when the install root of the Steam Edition was given). `None` when
    /// there is none.
    pub data_dir: Option<String>,
    pub edition_guess: EditionGuess,
    /// The language an import would use without `--language`: the one the install implies
    /// unambiguously (exactly one language executable, else exactly one language directory), or
    /// `en` when several languages are present, no executable names one and `en` is among them
    /// (then [`language_defaulted`](Self::language_defaulted) is true).
    pub language_guess: Option<String>,
    /// True when `language_guess` is the `en` default for an install that holds several
    /// languages and says nothing about which one is meant. The report then carries a warning
    /// that starts with [`LANGUAGE_DEFAULT_PREFIX`].
    pub language_defaulted: bool,
    /// Every language the install holds (executables `ff7_<xx>.exe`, `data/lang-<xx>` and the
    /// per-language field archives).
    pub languages: Vec<LanguageOption>,
    /// True when several languages are present and no single one is implied: `import-pc` then
    /// needs `--language`.
    pub language_selection_required: bool,
    pub modded: bool,
    pub mod_markers: Vec<ModMarker>,
    pub executables: Vec<Executable>,
    pub lang_dirs: Vec<String>,
    pub required_files: Vec<RequiredFile>,
    pub m1: Milestone,
    pub m2: Milestone,
    /// Every candidate file, tagged with its language (see [`WouldImport::language`]).
    pub would_import: Vec<WouldImport>,
    /// Total size of all candidates in `would_import`.
    pub would_import_bytes: u64,
    /// Total size of the candidates marked `in_default_import`: what `import-pc` without
    /// `--language` would copy (0 when it would refuse for lack of a language).
    pub default_import_bytes: u64,
    /// Things that make the install look like something other than the real game (empty files),
    /// and the language default (a warning starting with [`LANGUAGE_DEFAULT_PREFIX`]).
    pub warnings: Vec<String>,
    /// Candidate files that were left out because they are symbolic links leading out of the
    /// scanned directory. Absent from the report when there are none.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub refused_links: Vec<RefusedLink>,
    pub notes: Vec<&'static str>,
    /// The canonical install root of the Steam Edition the scanned directory belongs to, when it
    /// was identified (it can sit above the scanned directory). `import-pc` keeps its state
    /// directory out of it too. Not part of the report: it is a local path.
    #[serde(skip)]
    pub(crate) steam_install_root: Option<PathBuf>,
}

impl ScanReport {
    /// Decide which candidate files one import takes. Never mixes languages: the result holds the
    /// language-neutral files plus exactly one language's own files.
    ///
    /// - with `requested`: it must match a language the install holds (case-insensitively);
    ///   an install without any language marker takes the label as given;
    /// - without it: the guess (unambiguous, or the `en` default of an install that holds several
    ///   languages and says nothing about which one is meant), else `unknown` when there is
    ///   nothing to choose from, else [`LanguageError::Ambiguous`].
    pub fn select(&self, requested: Option<&str>) -> Result<Selection<'_>, LanguageError> {
        let available = || {
            self.languages
                .iter()
                .map(|l| l.language.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let requested = requested.map(str::trim).filter(|r| !r.is_empty());
        let language = match requested {
            Some(r) if self.languages.is_empty() => r.to_string(),
            Some(r) => match self
                .languages
                .iter()
                .find(|l| l.language.eq_ignore_ascii_case(r))
            {
                Some(l) => l.language.clone(),
                None => {
                    return Err(LanguageError::NotInInstall {
                        given: r.chars().take(40).collect(),
                        available: available(),
                    })
                }
            },
            None => match &self.language_guess {
                Some(g) => g.clone(),
                None if self.languages.len() > 1 => {
                    return Err(LanguageError::Ambiguous {
                        available: available(),
                    })
                }
                None => "unknown".to_string(),
            },
        };
        let files = self
            .would_import
            .iter()
            .filter(|w| match &w.language {
                None => true,
                Some(l) => l.eq_ignore_ascii_case(&language),
            })
            .collect();
        let left_out = self
            .languages
            .iter()
            .filter(|l| !l.language.eq_ignore_ascii_case(&language))
            .map(|l| l.language.clone())
            .collect();
        Ok(Selection {
            language,
            files,
            left_out,
        })
    }
}

/// Directory entry named `name` (exact match first, else ASCII-case-insensitive).
pub(crate) fn find_entry(dir: &Path, name: &str) -> Option<(String, PathBuf)> {
    let rd = fs::read_dir(dir).ok()?;
    let mut best: Option<(String, PathBuf)> = None;
    for ent in rd.flatten() {
        let actual = ent.file_name().to_string_lossy().into_owned();
        if actual == name {
            return Some((actual, ent.path()));
        }
        if actual.eq_ignore_ascii_case(name) {
            match &best {
                Some((b, _)) if *b <= actual => {}
                _ => best = Some((actual, ent.path())),
            }
        }
    }
    best
}

/// Case-insensitive lookup of `rel` (`/` separated) below `base`.
fn find_ci(base: &Path, rel: &str) -> Option<(PathBuf, String)> {
    let mut cur = base.to_path_buf();
    let mut actual_parts = Vec::new();
    for comp in rel.split('/') {
        let (actual, path) = find_entry(&cur, comp)?;
        actual_parts.push(actual);
        cur = path;
    }
    Some((cur, actual_parts.join("/")))
}

fn regular_file_size(path: &Path) -> Option<u64> {
    let m = fs::metadata(path).ok()?;
    m.is_file().then_some(m.len())
}

fn is_dir(path: &Path) -> bool {
    fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false)
}

/// [`regular_file_size`] for a file that an import may take: `None` (and a note in `refused`)
/// when it is a symbolic link, or sits behind one, that leads out of `canon_root`. `shown` is
/// the file's path relative to the scanned directory.
fn candidate_size(
    path: &Path,
    shown: &str,
    canon_root: &Path,
    refused: &mut Vec<RefusedLink>,
) -> Option<u64> {
    let size = regular_file_size(path)?;
    match fs::canonicalize(path) {
        Ok(real) if real.starts_with(canon_root) => Some(size),
        Ok(_) => {
            refused.push(RefusedLink {
                relative_path: shown.to_string(),
                reason: LINK_REFUSED,
            });
            None
        }
        Err(_) => None,
    }
}

/// The language code of a `lang-<code>` directory name (lower-cased), if it is a sane one:
/// 1 to 16 characters of `a-z 0-9 _ -`. Anything else (`lang-`, `lang-../x`, a 300-character
/// name) is not treated as a language directory, so its text never becomes a language label.
fn lang_dir_code(dir_name: &str) -> Option<String> {
    let lower = dir_name.to_ascii_lowercase();
    let code = lower.strip_prefix("lang-")?;
    let sane = (1..=16).contains(&code.len())
        && code
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-');
    sane.then(|| code.to_string())
}

/// `ff7.exe` -> Some(None) (classic executable), `ff7_en.exe` -> Some(Some("en")), else None.
fn classify_exe(name: &str) -> Option<Option<String>> {
    let lower = name.to_ascii_lowercase();
    if lower == "ff7.exe" {
        return Some(None);
    }
    let lang = lower.strip_prefix("ff7_")?.strip_suffix(".exe")?;
    (lang.len() == 2 && lang.chars().all(|c| c.is_ascii_lowercase()))
        .then(|| Some(lang.to_string()))
}

/// Mod markers in `root` (and 7thHeaven.exe nearby). `shown` is how `root` is spelled relative to
/// the scanned directory (`""` for that directory itself, else `ff7/workingdir/` and the like):
/// it prefixes the evidence text.
fn detect_mods(root: &Path, shown: &str) -> Vec<ModMarker> {
    let mut out = Vec::new();
    for (file, kind) in [("FFNx.dll", "FFNx"), ("FFNx.toml", "FFNx")] {
        if let Some((actual, _)) = find_entry(root, file) {
            out.push(ModMarker {
                kind,
                evidence: format!("{shown}{actual}"),
            });
        }
    }
    if let Some((actual, p)) = find_entry(root, "7thWorkshop") {
        if is_dir(&p) {
            out.push(ModMarker {
                kind: "7th Heaven",
                evidence: format!("{shown}{actual}/ (directory)"),
            });
        }
    }
    if let Some((actual, p)) = find_entry(root, "mods") {
        if is_dir(&p) {
            out.push(ModMarker {
                kind: "mods directory",
                evidence: format!("{shown}{actual}/ (directory)"),
            });
        }
    }
    // 7thHeaven.exe "nearby": the install root, its parent, or a 7th Heaven folder in either.
    let mut places: Vec<(PathBuf, String)> = vec![(root.to_path_buf(), shown.to_string())];
    if let Some(parent) = root.parent().filter(|p| !p.as_os_str().is_empty()) {
        places.push((parent.to_path_buf(), format!("{shown}../")));
    }
    let mut searched = Vec::new();
    for (base, prefix) in &places {
        searched.push((base.clone(), prefix.clone()));
        for folder in ["7th Heaven", "7thHeaven"] {
            if let Some((actual, p)) = find_entry(base, folder) {
                if is_dir(&p) {
                    searched.push((p, format!("{prefix}{actual}/")));
                }
            }
        }
    }
    for (base, prefix) in searched {
        if let Some((actual, p)) = find_entry(&base, "7thHeaven.exe") {
            if regular_file_size(&p).is_some() {
                out.push(ModMarker {
                    kind: "7th Heaven",
                    evidence: format!("{prefix}{actual}"),
                });
                break;
            }
        }
    }
    out
}

fn has_steam_hint(root: &Path) -> Option<String> {
    for f in ["steam_api.dll", "steam_api64.dll", "steam_appid.txt"] {
        if let Some((actual, _)) = find_entry(root, f) {
            return Some(format!("{actual} present"));
        }
    }
    let canon = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    canon
        .components()
        .any(|c| c.as_os_str().eq_ignore_ascii_case("steamapps"))
        .then(|| "install path is under a steamapps directory".to_string())
}

/// The game directory: the folder that holds `data/`.
struct GameDir {
    dir: PathBuf,
    /// From the scanned directory to `dir`: `""` when they are the same, else `ff7/workingdir` or
    /// the like. `/` separated, with the case found on disk.
    rel: String,
}

impl GameDir {
    /// `rest` (relative to the game directory) as a path relative to the scanned directory.
    fn join(&self, rest: &str) -> String {
        if self.rel.is_empty() {
            rest.to_string()
        } else {
            format!("{}/{rest}", self.rel)
        }
    }
}

fn holds_data(dir: &Path) -> bool {
    find_entry(dir, "data").is_some_and(|(_, p)| is_dir(&p))
}

/// Find the folder that holds `data/`: the scanned directory itself (the classic layouts, or the
/// Steam Edition's `ff7/workingdir` given directly), else `ff7/workingdir` below it (the Steam
/// Edition's install root), else `workingdir` below it (its `ff7` folder).
fn locate_game_dir(root: &Path) -> GameDir {
    if !holds_data(root) {
        for sub in ["ff7/workingdir", "workingdir"] {
            if let Some((dir, rel)) = find_ci(root, sub) {
                if is_dir(&dir) && holds_data(&dir) {
                    return GameDir { dir, rel };
                }
            }
        }
    }
    GameDir {
        dir: root.to_path_buf(),
        rel: String::new(),
    }
}

/// The Steam Edition install the scanned game directory belongs to.
struct SteamEditionFound {
    /// Canonical install root.
    install: PathBuf,
    /// How many directories above the scanned one it is (0: the scanned directory itself).
    levels_up: usize,
    /// Size of `FFVII.exe`, when the file layout is complete (`FFVII.exe`, `ff7/workingdir` and
    /// `ff7/resources/ff7_1.02` all present); `None` when it is not.
    exe_size: Option<u64>,
    /// Steam's appmanifest for appid 3837340 next to this install.
    manifest: ManifestLookup,
}

/// Look for the install root of the Steam Edition at the scanned directory and the two above it
/// (the scanned directory may be the install root, its `ff7` folder or `ff7/workingdir`). A
/// candidate counts when its `ff7/workingdir` IS the game directory and either the file layout
/// is complete or Steam's appmanifest for appid 3837340 vouches for the folder.
fn detect_steam_edition(canon_root: &Path, canon_game: &Path) -> Option<SteamEditionFound> {
    for (levels_up, install) in canon_root.ancestors().take(3).enumerate() {
        let Some((workingdir, _)) = find_ci(install, "ff7/workingdir") else {
            continue;
        };
        if fs::canonicalize(&workingdir).ok().as_deref() != Some(canon_game) {
            continue;
        }
        let exe_size = find_entry(install, "FFVII.exe")
            .and_then(|(_, p)| regular_file_size(&p))
            .filter(|_| find_ci(install, "ff7/resources/ff7_1.02").is_some());
        let manifest = find_app_manifest(install, STEAM_EDITION_APPID);
        if exe_size.is_some() || matches!(manifest, ManifestLookup::Matched(_)) {
            return Some(SteamEditionFound {
                install: install.to_path_buf(),
                levels_up,
                exe_size,
                manifest,
            });
        }
    }
    None
}

/// Steam's appmanifest for the 2013 release, for the scanned directory or one of the two above.
fn find_2013_manifest(canon_root: &Path) -> ManifestLookup {
    for install in canon_root.ancestors().take(3) {
        match find_app_manifest(install, STEAM_2013_APPID) {
            ManifestLookup::NotFound => {}
            found => return found,
        }
    }
    ManifestLookup::NotFound
}

/// What the file names say about the edition (input of [`guess_edition`]).
struct EditionFacts {
    has_classic_exe: bool,
    has_lang_exe: bool,
    has_lang_layout: bool,
    classic_data: bool,
    steam_hint: Option<String>,
}

/// One sentence on what an appmanifest that matched the install says.
fn manifest_basis(m: &SteamAppMatch) -> String {
    let build = match &m.manifest.buildid {
        Some(b) => format!("build {b}"),
        None => "no build id".to_string(),
    };
    format!(
        "Steam's {} (in the steamapps directory the install sits in) names appid {} and this install folder; {build}",
        m.file_name, m.manifest.appid
    )
}

/// The edition guess: from the file layout, and from Steam's appmanifest when one for this
/// install was found. Unverified either way: no game file is compared with a known release.
fn guess_edition(
    steam_edition: Option<&SteamEditionFound>,
    manifest_2013: &ManifestLookup,
    f: &EditionFacts,
) -> EditionGuess {
    // The Steam Edition: FFVII.exe + ff7/workingdir + ff7/resources/ff7_1.02 (medium), `high`
    // once Steam's appmanifest for appid 3837340 vouches for this very folder as well.
    if let Some(found) = steam_edition {
        let mut basis = vec![if found.exe_size.is_some() {
            "FFVII.exe, ff7/workingdir and ff7/resources/ff7_1.02 are all present: the layout of the Steam Edition (appid 3837340)".to_string()
        } else {
            "ff7/workingdir holds the data, but FFVII.exe and/or ff7/resources/ff7_1.02 is missing"
                .to_string()
        }];
        let (confidence, revision, steam) = match &found.manifest {
            ManifestLookup::Matched(m) => {
                basis.push(manifest_basis(m));
                (
                    if found.exe_size.is_some() {
                        "high"
                    } else {
                        "medium"
                    },
                    m.manifest.buildid.as_deref().map(build_revision),
                    Some(SteamEvidence::from_match(m)),
                )
            }
            ManifestLookup::Ignored { file_name, reason } => {
                basis.push(format!("{file_name} was found but ignored: {reason}"));
                ("medium", None, None)
            }
            ManifestLookup::NotFound => {
                basis.push("no appmanifest_3837340.acf for this install was found in a steamapps directory above it: the build is unknown and the confidence stays at medium".to_string());
                ("medium", None, None)
            }
        };
        return EditionGuess {
            edition: STEAM_EDITION_LABEL.to_string(),
            confidence,
            basis,
            verified: false,
            revision,
            steam,
        };
    }

    let mut basis = Vec::new();
    let (mut edition, mut confidence) = if f.has_lang_exe
        || (f.has_lang_layout && !f.has_classic_exe)
    {
        if f.has_lang_exe {
            basis.push("language executable ff7_<xx>.exe present (Steam 2013 style)".to_string());
        }
        if f.has_lang_layout {
            basis.push("data/lang-* directories present".to_string());
        }
        if let Some(h) = &f.steam_hint {
            basis.push(h.clone());
        }
        basis.push("the 2012 Square Enix store build may look identical [verify]".to_string());
        let strong = f.has_lang_exe && (f.has_lang_layout || f.steam_hint.is_some());
        (
            STEAM_2013_LABEL.to_string(),
            if strong { "medium" } else { "low" },
        )
    } else if f.has_classic_exe && !f.has_lang_layout {
        basis.push("ff7.exe present (1998 style)".to_string());
        if f.classic_data {
            basis.push("data/field/flevel.lgp in the classic layout".to_string());
        }
        (
            "PC 1998".to_string(),
            if f.classic_data { "medium" } else { "low" },
        )
    } else if f.has_classic_exe {
        basis
            .push("ff7.exe AND data/lang-* directories present: contradictory signals".to_string());
        ("PC unknown".to_string(), "low")
    } else {
        basis.push("no ff7.exe / ff7_<xx>.exe found".to_string());
        ("PC unknown".to_string(), "none")
    };

    // Steam's appmanifest for the 2013 release. It is evidence for the edition and raises the
    // confidence one step; it does NOT set the revision (the label stays `unknown` unless
    // --revision says otherwise), so the set ids of 2013 installs do not depend on it.
    let mut steam = None;
    match manifest_2013 {
        ManifestLookup::Matched(m) => {
            basis.push(format!(
                "{}; it is evidence for the edition only: the revision stays unknown unless --revision says otherwise",
                manifest_basis(m)
            ));
            if edition == STEAM_2013_LABEL {
                confidence = match confidence {
                    "low" => "medium",
                    "medium" => "high",
                    other => other,
                };
            } else {
                basis.push("the file names alone would not have said Steam 2013: the manifest decides, at low confidence".to_string());
                edition = STEAM_2013_LABEL.to_string();
                confidence = "low";
            }
            steam = Some(SteamEvidence::from_match(m));
        }
        ManifestLookup::Ignored { file_name, reason } => {
            basis.push(format!("{file_name} was found but ignored: {reason}"));
        }
        ManifestLookup::NotFound => {}
    }
    EditionGuess {
        edition,
        confidence,
        basis,
        verified: false,
        revision: None,
        steam,
    }
}

/// Scan `root` and describe what a PC import would take. Writes nothing.
///
/// `root` is the folder that holds `data/`, or the install root of the Steam Edition (or its
/// `ff7` folder), see the module docs.
pub fn scan_pc(root: &Path) -> Result<ScanReport, ScanError> {
    let meta = fs::metadata(root).map_err(|source| ScanError::Io {
        path: root.to_path_buf(),
        source,
    })?;
    if !meta.is_dir() {
        return Err(ScanError::NotADirectory(root.to_path_buf()));
    }

    // where the data is, and which Steam Edition install (if any) it belongs to
    let game = locate_game_dir(root);
    let canon_root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let canon_game = fs::canonicalize(&game.dir).unwrap_or_else(|_| game.dir.clone());
    let steam_edition = detect_steam_edition(&canon_root, &canon_game);
    // candidates that are links out of the scanned directory (see the module docs)
    let mut refused_links: Vec<RefusedLink> = Vec::new();

    // executables in the scanned directory
    let mut executables = Vec::new();
    if let Ok(rd) = fs::read_dir(root) {
        for ent in rd.flatten() {
            let name = ent.file_name().to_string_lossy().into_owned();
            if let Some(language) = classify_exe(&name) {
                if let Some(size) =
                    candidate_size(&ent.path(), &name, &canon_root, &mut refused_links)
                {
                    executables.push(Executable {
                        relative_path: name,
                        size,
                        language,
                    });
                }
            }
        }
    }
    executables.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));

    // data directory and language directories
    let data = find_entry(&game.dir, "data").filter(|(_, p)| is_dir(p));
    let mut lang_dirs: Vec<(String, PathBuf)> = Vec::new();
    if let Some((_, data_path)) = &data {
        if let Ok(rd) = fs::read_dir(data_path) {
            for ent in rd.flatten() {
                let name = ent.file_name().to_string_lossy().into_owned();
                if lang_dir_code(&name).is_some() && is_dir(&ent.path()) {
                    lang_dirs.push((name, ent.path()));
                }
            }
        }
    }
    lang_dirs.sort();

    // required files: classic location, every language directory, and the language variants
    let mut required_files = Vec::new();
    for w in WANTED {
        let mut locations = Vec::new();
        let rest = w.rel.strip_prefix("data/").unwrap_or(w.rel);
        if let Some((path, rel_in_game)) = find_ci(&game.dir, w.rel) {
            if let Some(size) = candidate_size(
                &path,
                &game.join(&rel_in_game),
                &canon_root,
                &mut refused_links,
            ) {
                locations.push(FoundFile::at(
                    &game,
                    &rel_in_game,
                    size,
                    "classic",
                    &rel_in_game,
                ));
            }
        }
        if let Some((data_name, _)) = &data {
            for (lang_name, lang_path) in &lang_dirs {
                if let Some((file_path, rel_in_lang)) = find_ci(lang_path, rest) {
                    let rel_in_game = format!("{data_name}/{lang_name}/{rel_in_lang}");
                    if let Some(size) = candidate_size(
                        &file_path,
                        &game.join(&rel_in_game),
                        &canon_root,
                        &mut refused_links,
                    ) {
                        let mut f = FoundFile::at(
                            &game,
                            &rel_in_game,
                            size,
                            &lang_name.to_ascii_lowercase(),
                            &rel_in_game,
                        );
                        f.language = lang_dir_code(lang_name);
                        locations.push(f);
                    }
                }
            }
        }
        // Language variants named next to the classic file (the Steam Edition's fflevel.lgp ...):
        // each is imported under the classic file's name in data/lang-<xx>/, where readers look
        // for a language's own file, and the classic file itself is then the English one.
        let dir = w.rel.rsplit_once('/').map_or("", |(d, _)| d);
        let mut variants = Vec::new();
        for v in w.variants {
            let Some((path, rel_in_game)) = find_ci(&game.dir, &format!("{dir}/{}", v.file)) else {
                continue;
            };
            let Some(size) = candidate_size(
                &path,
                &game.join(&rel_in_game),
                &canon_root,
                &mut refused_links,
            ) else {
                continue;
            };
            let logical = format!("data/lang-{}/{rest}", v.language);
            let mut f = FoundFile::at(&game, &rel_in_game, size, "classic", &logical);
            f.language = Some(v.language.to_string());
            f.language_inferred = true;
            f.variant_of = Some(w.name);
            variants.push(f);
        }
        if !variants.is_empty() {
            if let Some(classic) = locations.iter_mut().find(|l| l.layout == "classic") {
                classic.language = Some(CLASSIC_VARIANT_LANGUAGE.to_string());
                classic.language_inferred = true;
            }
            // last: a real data/lang-<xx>/ file of the same name wins when both exist
            locations.extend(variants);
        }
        required_files.push(RequiredFile {
            name: w.name,
            milestone: w.milestone,
            found: !locations.is_empty(),
            locations,
        });
    }
    let milestone = |label: &str| {
        let missing: Vec<&'static str> = required_files
            .iter()
            .filter(|r| r.milestone == label && !r.found)
            .map(|r| r.name)
            .collect();
        Milestone {
            ready: missing.is_empty(),
            missing,
        }
    };
    let m1 = milestone("M1");
    let m2 = milestone("M2");
    let mut notes = vec![
        "scan-pc lists directory entries and reads file metadata only, plus (when the install sits in a Steam library) Steam's small appmanifest_<appid>.acf text file; it never opens game files and writes nothing.",
        "Edition and language are guesses from file presence (and, for Steam installs, Steam's appmanifest): UNVERIFIED [verify]: no sizes or hashes were compared against known releases.",
        "The layout under data/lang-* is UNCERTAIN: every location found is listed and none is assumed to be canonical.",
    ];
    if !game.rel.is_empty() {
        notes.push("The game directory (the folder that holds data/) is below the scanned directory: see data_dir. Paths in this report are relative to the scanned directory; library paths (data/...) are relative to the game directory.");
    }
    if required_files
        .iter()
        .flat_map(|r| r.locations.iter())
        .any(|l| l.variant_of.is_some())
    {
        notes.push("Field archives fflevel/gflevel/sflevel/jfleve.lgp are taken as the French, German, Spanish and Japanese ones and flevel.lgp as the English one, from the file names alone [verify]. A non-English set stores its field archive as data/lang-<xx>/field/flevel.lgp, where readers look for it.");
    }
    if !m1.ready && find_ci(root, "field/flevel.lgp").is_some() {
        notes.push("field/flevel.lgp sits directly under this directory: it looks like the data directory itself; pass its parent (the install root) instead.");
    }

    // mods: in the scanned directory, in the game directory and in the Steam Edition install
    // root, so that every way of naming one install gives the same verdict
    let mut mod_markers = detect_mods(root, "");
    let mut also_look: Vec<(PathBuf, String)> = Vec::new();
    if !game.rel.is_empty() {
        also_look.push((game.dir.clone(), format!("{}/", game.rel)));
    }
    if let Some(s) = steam_edition.as_ref().filter(|s| s.levels_up > 0) {
        also_look.push((s.install.clone(), "../".repeat(s.levels_up)));
    }
    for (dir, shown) in also_look {
        for m in detect_mods(&dir, &shown) {
            if !mod_markers
                .iter()
                .any(|x| x.kind == m.kind && x.evidence == m.evidence)
            {
                mod_markers.push(m);
            }
        }
    }
    let modded = !mod_markers.is_empty();

    // language and edition guesses (file presence only, UNVERIFIED)
    let mut exe_langs: Vec<String> = executables
        .iter()
        .filter_map(|e| e.language.clone())
        .collect();
    exe_langs.sort();
    exe_langs.dedup();
    // languages the layout itself names: language directories and per-language field archives
    let mut layout_langs: Vec<String> = lang_dirs
        .iter()
        .filter_map(|(n, _)| lang_dir_code(n))
        .collect();
    layout_langs.extend(
        required_files
            .iter()
            .flat_map(|r| r.locations.iter())
            .filter(|l| l.language_inferred)
            .filter_map(|l| l.language.clone()),
    );
    layout_langs.sort();
    layout_langs.dedup();
    let (language_guess, language_defaulted) = match (exe_langs.as_slice(), layout_langs.as_slice())
    {
        ([one], _) => (Some(one.clone()), false),
        ([], [one]) => (Some(one.clone()), false),
        // several languages and nothing that says which one is meant: `en`, said out loud
        ([], many) if many.len() > 1 && many.iter().any(|l| l == "en") => {
            (Some("en".to_string()), true)
        }
        _ => (None, false),
    };
    // every language the install holds, with its evidence
    let mut by_language: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for e in &executables {
        if let Some(l) = &e.language {
            by_language
                .entry(l.clone())
                .or_default()
                .push(e.relative_path.clone());
        }
    }
    if let Some((data_name, _)) = &data {
        for (n, _) in &lang_dirs {
            if let Some(l) = lang_dir_code(n) {
                by_language
                    .entry(l)
                    .or_default()
                    .push(format!("{}/", game.join(&format!("{data_name}/{n}"))));
            }
        }
    }
    for loc in required_files
        .iter()
        .flat_map(|r| r.locations.iter())
        .filter(|l| l.language_inferred)
    {
        if let Some(l) = &loc.language {
            by_language
                .entry(l.clone())
                .or_default()
                .push(loc.relative_path.clone());
        }
    }
    let languages: Vec<LanguageOption> = by_language
        .into_iter()
        .map(|(language, evidence)| LanguageOption { language, evidence })
        .collect();
    let language_selection_required = language_guess.is_none() && languages.len() > 1;
    let has_classic_exe = executables.iter().any(|e| e.language.is_none());
    let has_lang_exe = executables.iter().any(|e| e.language.is_some());
    let has_lang_layout = !lang_dirs.is_empty();
    let classic_data = required_files
        .iter()
        .any(|r| r.name == "flevel.lgp" && r.locations.iter().any(|l| l.layout == "classic"));
    let steam_hint = has_steam_hint(root);
    let manifest_2013 = if steam_edition.is_none() {
        find_2013_manifest(&canon_root)
    } else {
        ManifestLookup::NotFound
    };
    let mut edition_guess = guess_edition(
        steam_edition.as_ref(),
        &manifest_2013,
        &EditionFacts {
            has_classic_exe,
            has_lang_exe,
            has_lang_layout,
            classic_data,
            steam_hint,
        },
    );

    // A real install never holds an empty flevel.lgp, char.lgp, ... or executable: empty files are
    // placeholders (or damage). Presence alone must not read as a recognised install.
    let steam_exe_size = steam_edition.as_ref().and_then(|s| s.exe_size);
    let mut empty_files: Vec<String> = required_files
        .iter()
        .flat_map(|r| r.locations.iter())
        .filter(|l| l.size == 0)
        .map(|l| l.relative_path.clone())
        .collect();
    empty_files.extend(
        executables
            .iter()
            .filter(|e| e.size == 0)
            .map(|e| e.relative_path.clone()),
    );
    if steam_exe_size == Some(0) {
        empty_files.push("FFVII.exe".to_string());
    }
    empty_files.sort();
    empty_files.dedup();
    let found_any_file = required_files.iter().any(|r| r.found)
        || !executables.is_empty()
        || steam_exe_size.is_some();
    let found_only_empty = found_any_file
        && required_files
            .iter()
            .flat_map(|r| r.locations.iter())
            .all(|l| l.size == 0)
        && executables.iter().all(|e| e.size == 0)
        && steam_exe_size.is_none_or(|s| s == 0);
    let mut warnings = Vec::new();
    if !empty_files.is_empty() {
        let cap = if found_only_empty { "none" } else { "low" };
        let rank = |c: &str| match c {
            "none" => 0,
            "low" => 1,
            _ => 2,
        };
        if rank(cap) < rank(edition_guess.confidence) {
            edition_guess.confidence = cap;
        }
        warnings.push(format!(
            "{} empty (0-byte) file(s): {}. A real install has none of these empty: placeholders or damage, not game data.",
            empty_files.len(),
            empty_files.iter().take(8).cloned().collect::<Vec<_>>().join(", ")
        ));
        edition_guess.basis.push(
            "empty (0-byte) files present: the edition guess rests on file names alone".to_string(),
        );
    }
    refused_links.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    refused_links.dedup();
    if !refused_links.is_empty() {
        warnings.push(format!(
            "{} file(s) that would have been imported are symbolic links that lead outside the scanned directory and were left out: {}. Links that stay inside the directory are followed; for these, scan the directory that really holds the files, or copy them into the install.",
            refused_links.len(),
            refused_links
                .iter()
                .take(8)
                .map(|l| l.relative_path.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if language_defaulted {
        warnings.push(format!(
            "{LANGUAGE_DEFAULT_PREFIX} \"en\": this install holds several languages ({}) and none was chosen; pass --language <code> to import another one (every language is a set of its own)",
            languages
                .iter()
                .map(|l| l.language.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    // what an import would take
    let mut would_import = Vec::new();
    for r in &required_files {
        let role = if r.milestone == "M1" { "m1" } else { "m2" };
        for loc in &r.locations {
            if let Some(logical) = &loc.logical_path {
                would_import.push(WouldImport {
                    logical_path: logical.clone(),
                    source_relative_path: loc.relative_path.clone(),
                    size: loc.size,
                    role,
                    language: loc.language.clone(),
                    language_inferred: loc.language_inferred,
                    variant_of: loc.variant_of,
                    in_default_import: false,
                });
            }
        }
    }
    for e in &executables {
        if let Ok(logical) = normalize_logical_path(&e.relative_path, Platform::Pc) {
            would_import.push(WouldImport {
                logical_path: logical,
                source_relative_path: e.relative_path.clone(),
                size: e.size,
                role: "identify",
                language: e.language.clone(),
                language_inferred: false,
                variant_of: None,
                in_default_import: false,
            });
        }
    }
    would_import.sort_by(|a, b| a.logical_path.cmp(&b.logical_path));
    would_import.dedup_by(|a, b| a.logical_path == b.logical_path);
    let would_import_bytes = would_import
        .iter()
        .fold(0u64, |a, w| a.saturating_add(w.size));

    if languages.len() > 1 {
        notes.push("This install holds several languages. A library set holds exactly one: an import takes the language-neutral files plus ONE language (pass --language <code>; run again for another). Files of different languages are never mixed.");
    }
    let mut report = ScanReport {
        tool: "fmv-import scan-pc",
        report_version: REPORT_VERSION,
        root: root.display().to_string(),
        read_only: true,
        data_dir: data.as_ref().map(|(n, _)| game.join(n)),
        edition_guess,
        language_guess,
        language_defaulted,
        languages,
        language_selection_required,
        modded,
        mod_markers,
        executables,
        lang_dirs: lang_dirs.into_iter().map(|(n, _)| n).collect(),
        required_files,
        m1,
        m2,
        would_import,
        would_import_bytes,
        default_import_bytes: 0,
        warnings,
        refused_links,
        notes,
        steam_install_root: steam_edition.map(|s| s.install),
    };
    // mark what `import-pc` without --language would take, using the very rule it applies
    let default_files: Option<Vec<String>> = report
        .select(None)
        .ok()
        .map(|sel| sel.files.iter().map(|w| w.logical_path.clone()).collect());
    if let Some(taken) = default_files {
        for w in &mut report.would_import {
            w.in_default_import = taken.contains(&w.logical_path);
        }
    }
    report.default_import_bytes = report
        .would_import
        .iter()
        .filter(|w| w.in_default_import)
        .fold(0u64, |a, w| a.saturating_add(w.size));
    Ok(report)
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// A placeholder file with a few synthetic bytes (a real install never holds empty ones).
    fn touch(root: &Path, rel: &str) {
        touch_with(root, rel, b"synthetic placeholder");
    }

    fn touch_with(root: &Path, rel: &str, bytes: &[u8]) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, bytes).unwrap();
    }

    /// Everything under `root` as (relative path, size, mtime), to prove a scan changed nothing.
    fn snapshot(root: &Path) -> BTreeMap<String, (u64, Option<std::time::SystemTime>)> {
        let mut out = BTreeMap::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in fs::read_dir(&d).unwrap().flatten() {
                let m = e.metadata().unwrap();
                out.insert(e.path().display().to_string(), (m.len(), m.modified().ok()));
                if m.is_dir() {
                    stack.push(e.path());
                }
            }
        }
        out
    }

    fn classic_tree(root: &Path) {
        // consistent directory case, mixed file case (a case-sensitive filesystem would otherwise
        // see "Data" and "data" as two different directories)
        for f in [
            "ff7.exe",
            "Data/Field/FLEVEL.LGP",
            "Data/Field/char.lgp",
            "Data/Battle/battle.lgp",
            "Data/Battle/scene.bin",
            "Data/Kernel/KERNEL.BIN",
            "Data/Kernel/kernel2.bin",
        ] {
            touch(root, f);
        }
    }

    fn names(r: &ScanReport) -> Vec<&str> {
        r.would_import
            .iter()
            .map(|w| w.logical_path.as_str())
            .collect()
    }

    #[test]
    fn classic_1998_layout_is_recognised() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("FF7");
        classic_tree(&root);
        let before = snapshot(&root);
        let r = scan_pc(&root).unwrap();
        assert_eq!(snapshot(&root), before, "scan must not change the tree");
        assert!(r.read_only);
        assert_eq!(r.edition_guess.edition, "PC 1998");
        assert_eq!(r.edition_guess.confidence, "medium");
        assert!(!r.edition_guess.verified);
        assert_eq!(r.language_guess, None);
        assert!(!r.modded && r.mod_markers.is_empty());
        assert!(r.m1.ready && r.m2.ready, "{:?} {:?}", r.m1, r.m2);
        assert!(r.lang_dirs.is_empty());
        assert_eq!(
            names(&r),
            [
                "data/battle/battle.lgp",
                "data/battle/scene.bin",
                "data/field/char.lgp",
                "data/field/flevel.lgp",
                "data/kernel/kernel.bin",
                "data/kernel/kernel2.bin",
                "ff7.exe"
            ]
        );
        // actual on-disk case is reported, logical paths are lowercase
        let flevel = r
            .required_files
            .iter()
            .find(|f| f.name == "flevel.lgp")
            .unwrap();
        assert_eq!(flevel.locations[0].relative_path, "Data/Field/FLEVEL.LGP");
        assert_eq!(flevel.locations[0].layout, "classic");
        // the report is JSON-serialisable
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json["edition_guess"]["verified"], false);
    }

    #[test]
    fn missing_files_and_milestones() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        touch(root, "ff7.exe");
        touch(root, "data/field/flevel.lgp");
        touch(root, "data/kernel/kernel.bin");
        let r = scan_pc(root).unwrap();
        assert!(!r.m1.ready);
        assert_eq!(r.m1.missing, ["char.lgp"]);
        assert!(!r.m2.ready);
        assert_eq!(r.m2.missing, ["battle.lgp", "scene.bin", "kernel2.bin"]);
        let flevel = r
            .required_files
            .iter()
            .find(|f| f.name == "flevel.lgp")
            .unwrap();
        assert!(flevel.found);
        let none = scan_pc(&tmp.path().join("data")).unwrap();
        assert!(!none.m1.ready);
        assert!(none.would_import.is_empty() || none.would_import.iter().all(|w| w.role != "m1"));
    }

    #[test]
    fn steam_style_language_layout_is_reported_not_assumed() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("steamapps/common/FINAL FANTASY VII");
        for f in [
            "ff7_en.exe",
            "data/lang-en/field/flevel.lgp",
            "data/lang-en/field/char.lgp",
            "data/lang-fr/field/flevel.lgp",
            "data/battle/scene.bin",
        ] {
            touch(&root, f);
        }
        let r = scan_pc(&root).unwrap();
        assert_eq!(r.edition_guess.edition, "Steam 2013");
        assert_eq!(r.edition_guess.confidence, "medium");
        assert_eq!(r.language_guess.as_deref(), Some("en"));
        assert_eq!(r.lang_dirs, ["lang-en", "lang-fr"]);
        let flevel = r
            .required_files
            .iter()
            .find(|f| f.name == "flevel.lgp")
            .unwrap();
        let layouts: Vec<&str> = flevel.locations.iter().map(|l| l.layout.as_str()).collect();
        assert_eq!(layouts, ["lang-en", "lang-fr"]);
        assert!(r.m1.ready);
        assert!(r.notes.iter().any(|n| n.contains("UNCERTAIN")));
        // every location found is REPORTED (none is assumed canonical) ...
        assert!(names(&r).contains(&"data/lang-fr/field/flevel.lgp"));
        assert!(names(&r).contains(&"data/lang-en/field/flevel.lgp"));
        assert!(names(&r).contains(&"data/battle/scene.bin"));
        // ... each tagged with its language (None = language-neutral)
        let lang_of = |p: &str| {
            r.would_import
                .iter()
                .find(|w| w.logical_path == p)
                .unwrap_or_else(|| panic!("{p} not listed"))
                .language
                .clone()
        };
        assert_eq!(lang_of("data/lang-fr/field/flevel.lgp").as_deref(), Some("fr"));
        assert_eq!(lang_of("data/lang-en/field/char.lgp").as_deref(), Some("en"));
        assert_eq!(lang_of("ff7_en.exe").as_deref(), Some("en"));
        assert_eq!(lang_of("data/battle/scene.bin"), None);
        // the languages the install holds, with the evidence
        let langs: Vec<(&str, &[String])> = r
            .languages
            .iter()
            .map(|l| (l.language.as_str(), l.evidence.as_slice()))
            .collect();
        assert_eq!(langs.len(), 2);
        assert_eq!(langs[0].0, "en");
        assert_eq!(langs[0].1, ["ff7_en.exe".to_string(), "data/lang-en/".to_string()]);
        assert_eq!(langs[1], ("fr", &["data/lang-fr/".to_string()][..]));
        // one executable language is implied: nothing to choose, and French stays out
        assert!(!r.language_selection_required);
        let sel = r.select(None).unwrap();
        assert_eq!(sel.language, "en");
        assert_eq!(sel.left_out, ["fr"]);
        assert!(sel.has_flevel());
        let picked: Vec<&str> = sel.files.iter().map(|w| w.logical_path.as_str()).collect();
        assert!(!picked.iter().any(|p| p.contains("lang-fr")), "{picked:?}");
        assert!(picked.contains(&"data/battle/scene.bin"), "neutral files are always taken");
        // what scan-pc lists as "would import" is what import-pc takes: the default selection
        let flagged: Vec<&str> = r
            .would_import
            .iter()
            .filter(|w| w.in_default_import)
            .map(|w| w.logical_path.as_str())
            .collect();
        assert_eq!(flagged, picked);
        assert!(!r.would_import.iter().find(|w| w.logical_path.contains("lang-fr")).unwrap().in_default_import);
        let flagged_bytes: u64 = r.would_import.iter().filter(|w| w.in_default_import).map(|w| w.size).sum();
        assert_eq!(r.default_import_bytes, flagged_bytes);
        assert!(r.default_import_bytes < r.would_import_bytes, "French is listed but not taken");
    }

    /// One library set holds one language: an install with several must say which one it means.
    #[test]
    fn several_languages_need_a_choice_and_a_choice_never_mixes() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("steam");
        for f in [
            "ff7.exe",
            "ff7_en.exe",
            "ff7_fr.exe",
            "Data/field/char.lgp",
            "Data/Lang-DE/field/flevel.lgp",
            "Data/lang-en/field/flevel.lgp",
            "Data/lang-fr/field/flevel.lgp",
        ] {
            touch(&root, f);
        }
        let r = scan_pc(&root).unwrap();
        assert_eq!(r.language_guess, None, "two executables: nothing is implied");
        assert!(r.language_selection_required);
        // no default import exists: nothing is flagged, and the bytes say 0 instead of a mix
        assert!(r.would_import.iter().all(|w| !w.in_default_import));
        assert_eq!(r.default_import_bytes, 0);
        assert!(r.would_import_bytes > 0);
        assert_eq!(
            r.languages.iter().map(|l| l.language.as_str()).collect::<Vec<_>>(),
            ["de", "en", "fr"],
            "directory names are read case-insensitively"
        );
        assert!(r.notes.iter().any(|n| n.contains("--language")));
        // without a choice: refused, naming the options
        let err = r.select(None).unwrap_err();
        assert!(matches!(&err, LanguageError::Ambiguous { available } if available == "de, en, fr"));
        assert!(err.to_string().contains("--language"));
        // a language that is not in the install: refused
        assert!(matches!(
            r.select(Some("ja")),
            Err(LanguageError::NotInInstall { .. })
        ));
        // a choice (any letter case, spaces) takes that language plus the neutral files, no others
        for ask in ["fr", "FR", " Fr "] {
            let sel = r.select(Some(ask)).unwrap();
            assert_eq!(sel.language, "fr");
            let picked: Vec<&str> = sel.files.iter().map(|w| w.logical_path.as_str()).collect();
            assert_eq!(
                picked,
                [
                    "data/field/char.lgp",
                    "data/lang-fr/field/flevel.lgp",
                    "ff7.exe",
                    "ff7_fr.exe"
                ],
                "{ask:?}"
            );
            assert_eq!(sel.left_out, ["de", "en"]);
        }
        let de = r.select(Some("de")).unwrap();
        assert!(de.has_flevel());
        assert!(de.files.iter().all(|w| !w.logical_path.contains("ff7_")
            || w.logical_path == "ff7.exe"));
    }

    #[test]
    fn only_sane_language_directory_names_count() {
        for (name, code) in [
            ("lang-en", Some("en")),
            ("LANG-FR", Some("fr")),
            ("Lang-pt_br", Some("pt_br")),
            ("lang-", None),
            ("lang-é", None),
            ("lang-a b", None),
            ("lang-..", None),
            ("lang-x/y", None),
            ("language-en", None),
            ("lang-aaaaaaaaaaaaaaaaa", None), // 17 characters
        ] {
            assert_eq!(lang_dir_code(name).as_deref(), code, "{name:?}");
        }
        // such directories are ignored: they are not language options and nothing is taken from them
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), "ff7_en.exe");
        touch(tmp.path(), "data/field/flevel.lgp");
        touch(tmp.path(), "data/lang-/field/flevel.lgp");
        let r = scan_pc(tmp.path()).unwrap();
        assert!(r.lang_dirs.is_empty(), "{:?}", r.lang_dirs);
        assert_eq!(r.language_guess.as_deref(), Some("en"));
        assert!(r.would_import.iter().all(|w| !w.logical_path.contains("lang-")));
    }

    #[test]
    fn a_language_without_a_field_archive_has_no_flevel() {
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), "ff7_en.exe");
        touch(tmp.path(), "ff7_fr.exe");
        touch(tmp.path(), "data/lang-fr/field/flevel.lgp");
        let r = scan_pc(tmp.path()).unwrap();
        assert!(r.select(Some("fr")).unwrap().has_flevel());
        assert!(!r.select(Some("en")).unwrap().has_flevel());
    }

    #[test]
    fn an_install_without_language_markers_takes_the_label_as_given() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("FF7");
        classic_tree(&root);
        let r = scan_pc(&root).unwrap();
        assert!(r.languages.is_empty() && !r.language_selection_required);
        assert_eq!(r.select(None).unwrap().language, "unknown");
        let sel = r.select(Some("fr")).unwrap();
        assert_eq!(sel.language, "fr");
        assert_eq!(sel.files.len(), r.would_import.len(), "everything is language-neutral");
        assert!(r.would_import.iter().all(|w| w.language.is_none()));
        // blank means "not given"
        assert_eq!(r.select(Some("  ")).unwrap().language, "unknown");
    }

    /// Empty files are placeholders (or damage): presence alone must not read as a real install.
    #[test]
    fn empty_placeholder_files_lower_the_confidence() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("FF7");
        for f in ["ff7.exe", "data/field/flevel.lgp", "data/field/char.lgp"] {
            touch_with(&root, f, b"");
        }
        let r = scan_pc(&root).unwrap();
        assert_eq!(r.edition_guess.edition, "PC 1998", "the guess itself is unchanged ...");
        assert_eq!(r.edition_guess.confidence, "none", "... but nothing supports it");
        assert!(!r.edition_guess.verified);
        assert!(r.warnings.iter().any(|w| w.contains("empty (0-byte)") && w.contains("flevel.lgp")));
        assert!(r.edition_guess.basis.iter().any(|b| b.contains("empty")));
        // a mix: still only low, never medium
        touch_with(&root, "data/battle/battle.lgp", b"not empty");
        let mixed = scan_pc(&root).unwrap();
        assert_eq!(mixed.edition_guess.confidence, "low");
        // the same tree with real-looking sizes is the normal case
        touch(&root, "ff7.exe");
        touch(&root, "data/field/flevel.lgp");
        touch(&root, "data/field/char.lgp");
        let full = scan_pc(&root).unwrap();
        assert_eq!(full.edition_guess.confidence, "medium");
        assert!(full.warnings.is_empty());
    }

    #[test]
    fn mods_are_detected() {
        for (marker, kind) in [
            ("FFNx.dll", "FFNx"),
            ("ffnx.toml", "FFNx"),
            ("7thWorkshop/profile.json", "7th Heaven"),
            ("mods/readme.txt", "mods directory"),
            ("7th Heaven/7thHeaven.exe", "7th Heaven"),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path().join("FF7");
            classic_tree(&root);
            touch(&root, marker);
            let r = scan_pc(&root).unwrap();
            assert!(r.modded, "{marker}");
            assert!(
                r.mod_markers.iter().any(|m| m.kind == kind),
                "{marker}: {:?}",
                r.mod_markers
            );
        }
        // 7thHeaven.exe next to the install (in its parent)
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("FF7");
        classic_tree(&root);
        touch(tmp.path(), "7thHeaven.exe");
        let r = scan_pc(&root).unwrap();
        assert!(r.modded);
        assert_eq!(r.mod_markers[0].evidence, "../7thHeaven.exe");
        // an unmodded tree is not flagged (nested so that its parent directory is ours alone)
        let clean = tempfile::tempdir().unwrap();
        let clean_root = clean.path().join("FF7");
        classic_tree(&clean_root);
        assert!(!scan_pc(&clean_root).unwrap().modded);
    }

    #[test]
    fn contradictory_and_missing_executables_lower_confidence() {
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), "ff7.exe");
        touch(tmp.path(), "data/lang-en/field/flevel.lgp");
        let r = scan_pc(tmp.path()).unwrap();
        assert_eq!(
            (r.edition_guess.edition.as_str(), r.edition_guess.confidence),
            ("PC unknown", "low")
        );
        let empty = tempfile::tempdir().unwrap();
        let r = scan_pc(empty.path()).unwrap();
        assert_eq!(
            (r.edition_guess.edition.as_str(), r.edition_guess.confidence),
            ("PC unknown", "none")
        );
        assert!(r.would_import.is_empty());
    }

    #[test]
    fn exe_names_are_classified_strictly() {
        assert_eq!(classify_exe("FF7.EXE"), Some(None));
        assert_eq!(classify_exe("ff7_en.exe"), Some(Some("en".to_string())));
        assert_eq!(classify_exe("FF7_DE.exe"), Some(Some("de".to_string())));
        for no in [
            "ff7_.exe",
            "ff7_eng.exe",
            "ff7_e1.exe",
            "ff7.dll",
            "ff7_en.exe.bak",
            "xff7.exe",
            "ff7_opengl.exe",
        ] {
            assert_eq!(classify_exe(no), None, "{no}");
        }
    }

    #[test]
    fn passing_the_data_directory_itself_gets_a_hint() {
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), "data/field/flevel.lgp");
        let wrong = scan_pc(&tmp.path().join("data")).unwrap();
        assert!(!wrong.m1.ready);
        assert!(
            wrong
                .notes
                .iter()
                .any(|n| n.contains("data directory itself")),
            "{:?}",
            wrong.notes
        );
        let right = scan_pc(tmp.path()).unwrap();
        assert!(!right
            .notes
            .iter()
            .any(|n| n.contains("data directory itself")));
    }

    #[test]
    fn bad_roots_are_errors() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(matches!(
            scan_pc(&tmp.path().join("nope")),
            Err(ScanError::Io { .. })
        ));
        touch(tmp.path(), "file.txt");
        assert!(matches!(
            scan_pc(&tmp.path().join("file.txt")),
            Err(ScanError::NotADirectory(_))
        ));
    }

    // -----------------------------------------------------------------------------------------
    // the Steam Edition (appid 3837340): install root, game directory, appmanifest, languages
    // -----------------------------------------------------------------------------------------

    use crate::synthetic::{
        steam_appmanifest, steam_edition_install, steam_edition_library, write_file,
        STEAM_EDITION_DIR,
    };

    /// What a default (English) import of the Steam Edition takes: the six files the owner's
    /// library set holds, as library paths.
    const OWNERS_SIX: [&str; 6] = [
        "data/battle/battle.lgp",
        "data/field/char.lgp",
        "data/field/flevel.lgp",
        "data/lang-en/battle/scene.bin",
        "data/lang-en/kernel/kernel.bin",
        "data/lang-en/kernel/kernel2.bin",
    ];

    /// Truncate every file below `root` to 0 bytes (placeholders that look like damage).
    fn empty_every_file(root: &Path) {
        let mut stack = vec![root.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in fs::read_dir(&d).unwrap().flatten() {
                if e.metadata().unwrap().is_dir() {
                    stack.push(e.path());
                } else {
                    fs::write(e.path(), b"").unwrap();
                }
            }
        }
    }

    #[test]
    fn the_steam_edition_install_root_is_identified_from_its_files_and_steams_appmanifest() {
        let tmp = tempfile::tempdir().unwrap();
        let install = steam_edition_library(tmp.path(), true);
        let before = snapshot(tmp.path());
        let r = scan_pc(&install).unwrap();
        assert_eq!(
            snapshot(tmp.path()),
            before,
            "a scan writes nothing, and Steam's appmanifest is left as it was"
        );
        assert!(r.read_only);
        assert_eq!(r.report_version, 2);

        // the labels import-pc will use (pinned: they are part of the set id and the fingerprint)
        let g = &r.edition_guess;
        assert_eq!(g.edition, "Steam Edition (appid 3837340)");
        assert_eq!(g.confidence, "high");
        assert!(
            !g.verified,
            "no game file was compared with a known release"
        );
        assert_eq!(g.revision.as_deref(), Some("build 21140793"));
        let steam = g.steam.as_ref().unwrap();
        assert_eq!(steam.appid, "3837340");
        assert_eq!(steam.buildid.as_deref(), Some("21140793"));
        assert_eq!(steam.installdir, STEAM_EDITION_DIR);
        assert_eq!(steam.manifest_file, "appmanifest_3837340.acf");
        assert!(g
            .basis
            .iter()
            .any(|b| b.contains("FFVII.exe") && b.contains("ff7/resources/ff7_1.02")));
        assert!(g
            .basis
            .iter()
            .any(|b| b.contains("appmanifest_3837340.acf") && b.contains("build 21140793")));
        let printed = serde_json::to_string(g).unwrap();
        assert!(
            !printed.contains(&tmp.path().display().to_string()),
            "the guess names no local path: {printed}"
        );

        // where the data was found, and what is there
        assert_eq!(r.data_dir.as_deref(), Some("ff7/workingdir/data"));
        assert!(r
            .notes
            .iter()
            .any(|n| n.contains("below the scanned directory")));
        assert!(!r.modded && r.mod_markers.is_empty());
        assert!(r.m1.ready && r.m2.ready, "{:?} {:?}", r.m1, r.m2);
        assert_eq!(
            r.lang_dirs,
            ["lang-de", "lang-en", "lang-es", "lang-fr", "lang-ja"]
        );
        assert!(
            r.executables.is_empty(),
            "FFVII.exe is evidence for the edition, not an ff7*.exe to import"
        );

        // no per-language executable and several languages: en by default, said out loud
        let codes: Vec<&str> = r.languages.iter().map(|l| l.language.as_str()).collect();
        assert_eq!(codes, ["de", "en", "es", "fr", "ja"]);
        assert_eq!(r.language_guess.as_deref(), Some("en"));
        assert!(r.language_defaulted && !r.language_selection_required);
        assert_eq!(r.warnings.len(), 1, "{:?}", r.warnings);
        let warning = &r.warnings[0];
        assert!(warning.starts_with(LANGUAGE_DEFAULT_PREFIX), "{warning}");
        assert!(
            warning.contains("\"en\"")
                && warning.contains("de, en, es, fr, ja")
                && warning.contains("--language"),
            "{warning}"
        );

        // the default import is exactly the six files of the owner's library set
        let sel = r.select(None).unwrap();
        assert_eq!(sel.language, "en");
        assert_eq!(sel.left_out, ["de", "es", "fr", "ja"]);
        let picked: Vec<&str> = sel.files.iter().map(|w| w.logical_path.as_str()).collect();
        assert_eq!(picked, OWNERS_SIX);
        let flagged: Vec<&str> = r
            .would_import
            .iter()
            .filter(|w| w.in_default_import)
            .map(|w| w.logical_path.as_str())
            .collect();
        assert_eq!(flagged, OWNERS_SIX);
        // library paths are relative to ff7/workingdir, source paths to the directory given
        let flevel = sel
            .files
            .iter()
            .find(|w| w.logical_path == "data/field/flevel.lgp")
            .unwrap();
        assert_eq!(
            flevel.source_relative_path,
            "ff7/workingdir/data/field/flevel.lgp"
        );
        assert_eq!(flevel.language.as_deref(), Some("en"));
        assert!(flevel.language_inferred && flevel.variant_of.is_none());
        // the report is JSON, and says where the field archive of each language comes from
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json["edition_guess"]["revision"], "build 21140793");
        assert_eq!(json["data_dir"], "ff7/workingdir/data");
    }

    #[test]
    fn every_way_of_naming_the_steam_edition_install_scans_alike() {
        let tmp = tempfile::tempdir().unwrap();
        let install = steam_edition_library(tmp.path(), true);
        let mut seen = Vec::new();
        for (dir, data_dir) in [
            (install.clone(), "ff7/workingdir/data"),
            (install.join("ff7"), "workingdir/data"),
            (install.join("ff7/workingdir"), "data"),
        ] {
            let r = scan_pc(&dir).unwrap();
            assert_eq!(r.data_dir.as_deref(), Some(data_dir), "{dir:?}");
            let g = &r.edition_guess;
            assert_eq!(g.edition, "Steam Edition (appid 3837340)", "{dir:?}");
            assert_eq!(g.confidence, "high", "{dir:?}");
            assert_eq!(g.revision.as_deref(), Some("build 21140793"), "{dir:?}");
            assert!(!r.modded, "{dir:?}");
            // every source path leads to a file below the directory that was given
            for w in &r.would_import {
                assert!(
                    dir.join(&w.source_relative_path).is_file(),
                    "{dir:?}: {}",
                    w.source_relative_path
                );
            }
            // and the library paths do not depend on how the install was named
            seen.push(
                r.would_import
                    .iter()
                    .map(|w| (w.logical_path.clone(), w.language.clone(), w.size))
                    .collect::<Vec<_>>(),
            );
        }
        assert_eq!(seen[0], seen[1]);
        assert_eq!(seen[0], seen[2]);
    }

    #[test]
    fn the_steam_edition_is_found_whatever_the_letter_case_of_its_folders() {
        let tmp = tempfile::tempdir().unwrap();
        let install = steam_edition_library(tmp.path(), true);
        let at = |p: &str| install.join(p);
        fs::rename(at("ff7/workingdir/data"), at("ff7/workingdir/DATA")).unwrap();
        fs::rename(at("ff7/workingdir"), at("ff7/WorkingDir")).unwrap();
        fs::rename(at("ff7"), at("FF7")).unwrap();
        fs::rename(at("FFVII.exe"), at("ffvii.EXE")).unwrap();
        let r = scan_pc(&install).unwrap();
        assert_eq!(r.edition_guess.edition, "Steam Edition (appid 3837340)");
        assert_eq!(r.edition_guess.confidence, "high");
        assert_eq!(
            r.data_dir.as_deref(),
            Some("FF7/WorkingDir/DATA"),
            "the case found on disk is what the report shows"
        );
        // library paths are lower case whatever the disk says, and lead back to real files
        let sel = r.select(None).unwrap();
        let picked: Vec<&str> = sel.files.iter().map(|w| w.logical_path.as_str()).collect();
        assert_eq!(picked, OWNERS_SIX);
        for w in &sel.files {
            assert!(install.join(&w.source_relative_path).is_file());
        }
    }

    #[test]
    fn without_the_appmanifest_the_steam_edition_is_medium_and_its_build_unknown() {
        let tmp = tempfile::tempdir().unwrap();
        let install = steam_edition_library(tmp.path(), false);
        let r = scan_pc(&install).unwrap();
        let g = &r.edition_guess;
        assert_eq!(g.edition, "Steam Edition (appid 3837340)");
        assert_eq!(g.confidence, "medium");
        assert_eq!(g.revision, None);
        assert!(g.steam.is_none());
        assert!(g
            .basis
            .iter()
            .any(|b| b.contains("no appmanifest_3837340.acf")));
        // a copy of the install outside any steamapps directory has no manifest either
        let copy = tempfile::tempdir().unwrap();
        let copied = copy.path().join(STEAM_EDITION_DIR);
        steam_edition_install(&copied);
        let r = scan_pc(&copied).unwrap();
        assert_eq!(r.edition_guess.edition, "Steam Edition (appid 3837340)");
        assert_eq!(r.edition_guess.confidence, "medium");
        assert_eq!(r.edition_guess.revision, None);
        // ... and the data folder alone, away from the install, is only the old guess
        let bare = tempfile::tempdir().unwrap();
        for f in [
            "data/field/flevel.lgp",
            "data/lang-en/battle/scene.bin",
            "data/lang-fr/battle/scene.bin",
        ] {
            touch(bare.path(), f);
        }
        let r = scan_pc(bare.path()).unwrap();
        assert_eq!(r.edition_guess.edition, "Steam 2013");
        assert_eq!(r.edition_guess.confidence, "low");
    }

    #[test]
    fn the_appmanifest_alone_names_the_steam_edition_at_medium_confidence() {
        let tmp = tempfile::tempdir().unwrap();
        let install = steam_edition_library(tmp.path(), true);
        fs::remove_file(install.join("FFVII.exe")).unwrap();
        fs::remove_dir_all(install.join("ff7/resources")).unwrap();
        let r = scan_pc(&install).unwrap();
        let g = &r.edition_guess;
        assert_eq!(g.edition, "Steam Edition (appid 3837340)");
        assert_eq!(
            g.confidence, "medium",
            "high needs the files AND the manifest"
        );
        assert_eq!(g.revision.as_deref(), Some("build 21140793"));
        assert!(
            g.basis.iter().any(|b| b.contains("missing")),
            "{:?}",
            g.basis
        );
        // FFVII.exe and ff7/workingdir without the resources and without a manifest: not enough
        let tmp = tempfile::tempdir().unwrap();
        let install = steam_edition_library(tmp.path(), false);
        fs::remove_dir_all(install.join("ff7/resources")).unwrap();
        let r = scan_pc(&install).unwrap();
        assert_ne!(r.edition_guess.edition, "Steam Edition (appid 3837340)");
        assert_eq!(
            r.data_dir.as_deref(),
            Some("ff7/workingdir/data"),
            "the data is still found"
        );
    }

    #[test]
    fn a_manifest_that_does_not_vouch_for_this_folder_is_ignored() {
        for (appid, installdir, why) in [
            ("3837340", "SOME OTHER FOLDER", "installdir"),
            ("39140", STEAM_EDITION_DIR, "appid 39140"),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let install = steam_edition_library(tmp.path(), false);
            write_file(
                &tmp.path().join("steamapps"),
                "appmanifest_3837340.acf",
                steam_appmanifest(appid, installdir, "999").as_bytes(),
            );
            let r = scan_pc(&install).unwrap();
            let g = &r.edition_guess;
            assert_eq!(g.confidence, "medium", "{why}");
            assert_eq!(g.revision, None, "{why}");
            assert!(g.steam.is_none(), "{why}");
            assert!(
                g.basis
                    .iter()
                    .any(|b| b.contains("ignored") && b.contains(why)),
                "{why}: {:?}",
                g.basis
            );
        }
        // a build id that is not plain digits never becomes a label; the manifest still vouches
        let tmp = tempfile::tempdir().unwrap();
        let install = steam_edition_library(tmp.path(), false);
        write_file(
            &tmp.path().join("steamapps"),
            "appmanifest_3837340.acf",
            steam_appmanifest("3837340", STEAM_EDITION_DIR, "12 ab/../x").as_bytes(),
        );
        let g = scan_pc(&install).unwrap().edition_guess;
        assert_eq!((g.confidence, g.revision), ("high", None));
        assert!(
            g.basis.iter().any(|b| b.contains("no build id")),
            "{:?}",
            g.basis
        );
    }

    /// Empty placeholders are not a real install, whatever else says it is one: they cap even a
    /// `high` guess, while the labels stay what the evidence says.
    #[test]
    fn empty_placeholders_cap_the_steam_editions_confidence_even_with_the_appmanifest() {
        let tmp = tempfile::tempdir().unwrap();
        let install = steam_edition_library(tmp.path(), true);
        assert_eq!(scan_pc(&install).unwrap().edition_guess.confidence, "high");
        // only the executable is a placeholder
        fs::write(install.join("FFVII.exe"), b"").unwrap();
        let r = scan_pc(&install).unwrap();
        assert_eq!(r.edition_guess.confidence, "low");
        assert!(
            r.warnings
                .iter()
                .any(|w| w.contains("empty (0-byte)") && w.contains("FFVII.exe")),
            "{:?}",
            r.warnings
        );
        assert!(r.edition_guess.basis.iter().any(|b| b.contains("empty")));
        // every file is one
        empty_every_file(&install);
        let r = scan_pc(&install).unwrap();
        assert_eq!(r.edition_guess.confidence, "none");
        assert_eq!(r.edition_guess.edition, "Steam Edition (appid 3837340)");
        assert_eq!(r.edition_guess.revision.as_deref(), Some("build 21140793"));
        assert!(!r.edition_guess.verified);
        // the language default is still reported next to the empty-file warning
        assert!(r.warnings.iter().any(|w| w.contains("empty (0-byte)")));
        assert!(r
            .warnings
            .iter()
            .any(|w| w.starts_with(LANGUAGE_DEFAULT_PREFIX)));
    }

    #[test]
    fn the_field_archive_of_each_steam_edition_language_is_its_own_variant() {
        let tmp = tempfile::tempdir().unwrap();
        let install = steam_edition_library(tmp.path(), true);
        let r = scan_pc(&install).unwrap();
        assert!(r
            .notes
            .iter()
            .any(|n| n.contains("fflevel/gflevel/sflevel/jfleve")));
        for (lang, file) in [
            ("fr", "fflevel.lgp"),
            ("de", "gflevel.lgp"),
            ("es", "sflevel.lgp"),
            ("ja", "jfleve.lgp"),
        ] {
            let sel = r.select(Some(lang)).unwrap();
            let own = format!("data/lang-{lang}/field/flevel.lgp");
            let picked: Vec<String> = sel.files.iter().map(|w| w.logical_path.clone()).collect();
            assert_eq!(
                picked,
                [
                    "data/battle/battle.lgp".to_string(),
                    "data/field/char.lgp".to_string(),
                    format!("data/lang-{lang}/battle/scene.bin"),
                    own.clone(),
                    format!("data/lang-{lang}/kernel/kernel.bin"),
                    format!("data/lang-{lang}/kernel/kernel2.bin"),
                ],
                "{lang}: the language's own files and the neutral ones, never the English archive"
            );
            let f = sel.files.iter().find(|w| w.logical_path == own).unwrap();
            assert_eq!(
                f.source_relative_path,
                format!("ff7/workingdir/data/field/{file}"),
                "{lang}"
            );
            assert_eq!(f.variant_of, Some("flevel.lgp"));
            assert!(f.language_inferred);
            assert_eq!(f.language.as_deref(), Some(lang));
            assert!(sel.has_flevel());
        }
        // an explicit English choice is the default import
        let en = r.select(Some("EN")).unwrap();
        assert_eq!(
            en.files
                .iter()
                .map(|w| w.logical_path.as_str())
                .collect::<Vec<_>>(),
            OWNERS_SIX
        );
        // a language without a field archive of its own is refused by import-pc, not given the English one
        let tmp = tempfile::tempdir().unwrap();
        let install = steam_edition_library(tmp.path(), true);
        fs::remove_file(install.join("ff7/workingdir/data/field/sflevel.lgp")).unwrap();
        let r = scan_pc(&install).unwrap();
        assert!(!r.select(Some("es")).unwrap().has_flevel());
        assert!(r.select(Some("fr")).unwrap().has_flevel());
    }

    #[test]
    fn a_language_directorys_own_field_archive_beats_a_variant_of_the_same_library_path() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("steam");
        for f in [
            "ff7_en.exe",
            "data/field/flevel.lgp",
            "data/field/fflevel.lgp",
            "data/lang-fr/field/flevel.lgp",
            "data/lang-en/battle/scene.bin",
        ] {
            touch(&root, f);
        }
        let r = scan_pc(&root).unwrap();
        let fr = r.select(Some("fr")).unwrap();
        let f = fr
            .files
            .iter()
            .find(|w| w.logical_path == "data/lang-fr/field/flevel.lgp")
            .unwrap();
        assert_eq!(f.source_relative_path, "data/lang-fr/field/flevel.lgp");
        assert_eq!(f.variant_of, None);
        assert_eq!(
            r.would_import
                .iter()
                .filter(|w| w.logical_path == "data/lang-fr/field/flevel.lgp")
                .count(),
            1
        );
        // an executable that names a language still decides, defaults do not come into it
        assert_eq!(r.language_guess.as_deref(), Some("en"));
        assert!(!r.language_defaulted);
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    }

    #[test]
    fn several_language_directories_default_to_en_only_when_nothing_else_decides() {
        // several language directories, no executable, en among them: en, with a warning
        let tmp = tempfile::tempdir().unwrap();
        for lang in ["en", "fr", "de"] {
            touch(tmp.path(), &format!("data/lang-{lang}/field/flevel.lgp"));
        }
        let r = scan_pc(tmp.path()).unwrap();
        assert_eq!(r.language_guess.as_deref(), Some("en"));
        assert!(r.language_defaulted && !r.language_selection_required);
        assert!(r
            .warnings
            .iter()
            .any(|w| w.starts_with(LANGUAGE_DEFAULT_PREFIX) && w.contains("de, en, fr")));
        let sel = r.select(None).unwrap();
        assert_eq!(
            (sel.language.as_str(), sel.left_out.as_slice()),
            ("en", ["de".to_string(), "fr".to_string()].as_slice())
        );
        // the choice stays open, and another language is a set of its own
        assert_eq!(r.select(Some("fr")).unwrap().language, "fr");
        assert!(r
            .would_import
            .iter()
            .filter(|w| w.in_default_import)
            .all(|w| !w.logical_path.contains("lang-fr")));

        // without en there is nothing to default to: the install must say
        let tmp = tempfile::tempdir().unwrap();
        for lang in ["fr", "de"] {
            touch(tmp.path(), &format!("data/lang-{lang}/field/flevel.lgp"));
        }
        let r = scan_pc(tmp.path()).unwrap();
        assert_eq!(r.language_guess, None);
        assert!(r.language_selection_required && !r.language_defaulted);
        assert!(matches!(
            r.select(None),
            Err(LanguageError::Ambiguous { .. })
        ));

        // an executable that names one language decides it, with no default involved
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), "ff7_fr.exe");
        for lang in ["en", "fr"] {
            touch(tmp.path(), &format!("data/lang-{lang}/field/flevel.lgp"));
        }
        let r = scan_pc(tmp.path()).unwrap();
        assert_eq!(r.language_guess.as_deref(), Some("fr"));
        assert!(!r.language_defaulted);
        assert!(r.warnings.is_empty());
    }

    #[test]
    fn mod_markers_give_the_same_verdict_whichever_way_the_install_is_named() {
        for (marker, expect) in [
            ("FFNx.dll", true),
            ("7thWorkshop/profile.json", true),
            ("ff7/workingdir/mods/readme.txt", true),
            ("ff7/workingdir/FFNx.toml", true),
            ("ff7/7thHeaven.exe", true),
            ("ff7/resources/readme.txt", false),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let install = steam_edition_library(tmp.path(), true);
            touch(&install, marker);
            for dir in [
                install.clone(),
                install.join("ff7"),
                install.join("ff7/workingdir"),
            ] {
                assert_eq!(
                    scan_pc(&dir).unwrap().modded,
                    expect,
                    "{marker} seen from {dir:?}"
                );
            }
        }
    }

    #[test]
    fn steam_2013_is_named_by_its_appmanifest_and_its_revision_stays_unknown() {
        let tmp = tempfile::tempdir().unwrap();
        let apps = tmp.path().join("steamapps");
        let install = apps.join("common").join("FINAL FANTASY VII");
        for f in [
            "ff7_en.exe",
            "data/lang-en/field/flevel.lgp",
            "data/lang-en/field/char.lgp",
            "data/battle/scene.bin",
        ] {
            touch(&install, f);
        }
        // the file layout alone: medium
        let plain = scan_pc(&install).unwrap();
        assert_eq!(
            (
                plain.edition_guess.edition.as_str(),
                plain.edition_guess.confidence
            ),
            ("Steam 2013", "medium")
        );
        assert!(plain.edition_guess.steam.is_none());
        // with Steam's record of appid 39140: high, the build is evidence, the revision is not set
        write_file(
            &apps,
            "appmanifest_39140.acf",
            steam_appmanifest("39140", "FINAL FANTASY VII", "5012345").as_bytes(),
        );
        let r = scan_pc(&install).unwrap();
        let g = &r.edition_guess;
        assert_eq!((g.edition.as_str(), g.confidence), ("Steam 2013", "high"));
        assert_eq!(
            g.revision, None,
            "a 2013 set keeps the revision label it always had"
        );
        let steam = g.steam.as_ref().unwrap();
        assert_eq!(
            (steam.appid.as_str(), steam.buildid.as_deref()),
            ("39140", Some("5012345"))
        );
        assert!(g.basis.iter().any(|b| b.contains("appmanifest_39140.acf")));
        assert!(!serde_json::to_string(g)
            .unwrap()
            .contains(&tmp.path().display().to_string()));

        // the manifest decides the edition when the files alone would say something else
        let other = tempfile::tempdir().unwrap();
        let apps = other.path().join("steamapps");
        let install = apps.join("common").join("FINAL FANTASY VII");
        classic_tree(&install);
        write_file(
            &apps,
            "appmanifest_39140.acf",
            steam_appmanifest("39140", "FINAL FANTASY VII", "5012345").as_bytes(),
        );
        let g = scan_pc(&install).unwrap().edition_guess;
        assert_eq!((g.edition.as_str(), g.confidence), ("Steam 2013", "low"));
        assert!(
            g.basis.iter().any(|b| b.contains("would not have said")),
            "{:?}",
            g.basis
        );
    }
}
