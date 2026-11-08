//! Read-only scan of an FF7 PC install.
//!
//! The scan only lists directories and reads file metadata (names, sizes). It never opens a
//! file's contents and never writes anything, anywhere. Edition and language guesses come from
//! file presence alone and are UNVERIFIED; the layout under `data/lang-*` is uncertain, so every
//! location found is reported and none is assumed to be canonical.
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

pub const REPORT_VERSION: u32 = 1;

struct Wanted {
    name: &'static str,
    /// Classic layout, relative to the install root, `/` separated.
    rel: &'static str,
    milestone: &'static str,
}

const WANTED: &[Wanted] = &[
    Wanted {
        name: "flevel.lgp",
        rel: "data/field/flevel.lgp",
        milestone: "M1",
    },
    Wanted {
        name: "char.lgp",
        rel: "data/field/char.lgp",
        milestone: "M1",
    },
    Wanted {
        name: "battle.lgp",
        rel: "data/battle/battle.lgp",
        milestone: "M2",
    },
    Wanted {
        name: "scene.bin",
        rel: "data/battle/scene.bin",
        milestone: "M2",
    },
    Wanted {
        name: "kernel.bin",
        rel: "data/kernel/kernel.bin",
        milestone: "M2",
    },
    Wanted {
        name: "kernel2.bin",
        rel: "data/kernel/kernel2.bin",
        milestone: "M2",
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
    /// Relative to the install root, with the case found on disk.
    pub relative_path: String,
    pub size: u64,
    /// `classic` (`data/...`) or the language directory name (`lang-en`).
    pub layout: String,
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
    /// `none`, `low` or `medium`. Never higher: nothing is verified against known releases.
    pub confidence: &'static str,
    pub basis: Vec<String>,
    /// Always false: the guess is not checked against hashes of known releases.
    pub verified: bool,
}

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
    pub edition_guess: EditionGuess,
    /// The language an import would use without `--language`, when the install says so
    /// unambiguously (exactly one language executable, else exactly one language directory).
    pub language_guess: Option<String>,
    /// Every language the install holds (executables `ff7_<xx>.exe` and `data/lang-<xx>`).
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
    /// Things that make the install look like something other than the real game (empty files).
    pub warnings: Vec<String>,
    pub notes: Vec<&'static str>,
}

impl ScanReport {
    /// Decide which candidate files one import takes. Never mixes languages: the result holds the
    /// language-neutral files plus exactly one language's own files.
    ///
    /// - with `requested`: it must match a language the install holds (case-insensitively);
    ///   an install without any language marker takes the label as given;
    /// - without it: the unambiguous guess, else `unknown` when there is nothing to choose from,
    ///   else [`LanguageError::Ambiguous`].
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
fn find_entry(dir: &Path, name: &str) -> Option<(String, PathBuf)> {
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

fn detect_mods(root: &Path) -> Vec<ModMarker> {
    let mut out = Vec::new();
    for (file, kind) in [("FFNx.dll", "FFNx"), ("FFNx.toml", "FFNx")] {
        if let Some((actual, _)) = find_entry(root, file) {
            out.push(ModMarker {
                kind,
                evidence: actual,
            });
        }
    }
    if let Some((actual, p)) = find_entry(root, "7thWorkshop") {
        if is_dir(&p) {
            out.push(ModMarker {
                kind: "7th Heaven",
                evidence: format!("{actual}/ (directory)"),
            });
        }
    }
    if let Some((actual, p)) = find_entry(root, "mods") {
        if is_dir(&p) {
            out.push(ModMarker {
                kind: "mods directory",
                evidence: format!("{actual}/ (directory)"),
            });
        }
    }
    // 7thHeaven.exe "nearby": the install root, its parent, or a 7th Heaven folder in either.
    let mut places: Vec<(PathBuf, String)> = vec![(root.to_path_buf(), String::new())];
    if let Some(parent) = root.parent().filter(|p| !p.as_os_str().is_empty()) {
        places.push((parent.to_path_buf(), "../".to_string()));
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
    for f in ["steam_api.dll", "steam_appid.txt"] {
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

/// Scan `root` and describe what a PC import would take. Writes nothing.
pub fn scan_pc(root: &Path) -> Result<ScanReport, ScanError> {
    let meta = fs::metadata(root).map_err(|source| ScanError::Io {
        path: root.to_path_buf(),
        source,
    })?;
    if !meta.is_dir() {
        return Err(ScanError::NotADirectory(root.to_path_buf()));
    }

    // executables in the install root
    let mut executables = Vec::new();
    if let Ok(rd) = fs::read_dir(root) {
        for ent in rd.flatten() {
            let name = ent.file_name().to_string_lossy().into_owned();
            if let Some(language) = classify_exe(&name) {
                if let Some(size) = regular_file_size(&ent.path()) {
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
    let data = find_entry(root, "data").filter(|(_, p)| is_dir(p));
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

    // required files: classic location plus every language directory
    let mut required_files = Vec::new();
    for w in WANTED {
        let mut locations = Vec::new();
        if let Some((path, rel_from_root)) = find_ci(root, w.rel) {
            if let Some(size) = regular_file_size(&path) {
                locations.push(FoundFile {
                    relative_path: rel_from_root,
                    size,
                    layout: "classic".to_string(),
                });
            }
        }
        if let Some((data_name, _)) = &data {
            let rest = w.rel.strip_prefix("data/").unwrap_or(w.rel);
            for (lang_name, lang_path) in &lang_dirs {
                if let Some((file_path, rel_in_lang)) = find_ci(lang_path, rest) {
                    if let Some(size) = regular_file_size(&file_path) {
                        locations.push(FoundFile {
                            relative_path: format!("{data_name}/{lang_name}/{rel_in_lang}"),
                            size,
                            layout: lang_name.to_ascii_lowercase(),
                        });
                    }
                }
            }
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
        "scan-pc lists directory entries and reads file metadata only; it never opens file contents and writes nothing.",
        "Edition and language are guesses from file presence alone and are UNVERIFIED [verify]: no sizes or hashes were compared against known releases.",
        "The layout under data/lang-* is UNCERTAIN: every location found is listed and none is assumed to be canonical.",
    ];
    if !m1.ready && find_ci(root, "field/flevel.lgp").is_some() {
        notes.push("field/flevel.lgp sits directly under this directory: it looks like the data directory itself; pass its parent (the install root) instead.");
    }

    // mods
    let mod_markers = detect_mods(root);
    let modded = !mod_markers.is_empty();

    // language and edition guesses (file presence only, UNVERIFIED)
    let mut exe_langs: Vec<String> = executables
        .iter()
        .filter_map(|e| e.language.clone())
        .collect();
    exe_langs.sort();
    exe_langs.dedup();
    let dir_langs: Vec<String> = lang_dirs
        .iter()
        .filter_map(|(n, _)| lang_dir_code(n))
        .collect();
    let language_guess = match (exe_langs.as_slice(), dir_langs.as_slice()) {
        ([one], _) => Some(one.clone()),
        ([], [one]) => Some(one.clone()),
        _ => None,
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
                    .push(format!("{data_name}/{n}/"));
            }
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

    let mut basis = Vec::new();
    let (edition, confidence) = if has_lang_exe || (has_lang_layout && !has_classic_exe) {
        if has_lang_exe {
            basis.push("language executable ff7_<xx>.exe present (Steam 2013 style)".to_string());
        }
        if has_lang_layout {
            basis.push("data/lang-* directories present".to_string());
        }
        if let Some(h) = &steam_hint {
            basis.push(h.clone());
        }
        basis.push("the 2012 Square Enix store build may look identical [verify]".to_string());
        let strong = has_lang_exe && (has_lang_layout || steam_hint.is_some());
        (
            "Steam 2013".to_string(),
            if strong { "medium" } else { "low" },
        )
    } else if has_classic_exe && !has_lang_layout {
        basis.push("ff7.exe present (1998 style)".to_string());
        if classic_data {
            basis.push("data/field/flevel.lgp in the classic layout".to_string());
        }
        (
            "PC 1998".to_string(),
            if classic_data { "medium" } else { "low" },
        )
    } else if has_classic_exe {
        basis
            .push("ff7.exe AND data/lang-* directories present: contradictory signals".to_string());
        ("PC unknown".to_string(), "low")
    } else {
        basis.push("no ff7.exe / ff7_<xx>.exe found".to_string());
        ("PC unknown".to_string(), "none")
    };

    // A real install never holds an empty flevel.lgp, char.lgp, ... or executable: empty files are
    // placeholders (or damage). Presence alone must not read as a recognised install.
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
    empty_files.sort();
    empty_files.dedup();
    let found_any_file = required_files.iter().any(|r| r.found) || !executables.is_empty();
    let found_only_empty = found_any_file
        && required_files
            .iter()
            .flat_map(|r| r.locations.iter())
            .all(|l| l.size == 0)
        && executables.iter().all(|e| e.size == 0);
    let mut warnings = Vec::new();
    let mut confidence = confidence;
    if !empty_files.is_empty() {
        let cap = if found_only_empty { "none" } else { "low" };
        let rank = |c: &str| match c {
            "none" => 0,
            "low" => 1,
            _ => 2,
        };
        if rank(cap) < rank(confidence) {
            confidence = cap;
        }
        warnings.push(format!(
            "{} empty (0-byte) file(s): {}. A real install has none of these empty: placeholders or damage, not game data.",
            empty_files.len(),
            empty_files.iter().take(8).cloned().collect::<Vec<_>>().join(", ")
        ));
        basis.push("empty (0-byte) files present: the edition guess rests on file names alone".to_string());
    }

    // what an import would take
    let mut would_import = Vec::new();
    for r in &required_files {
        let role = if r.milestone == "M1" { "m1" } else { "m2" };
        for loc in &r.locations {
            if let Ok(logical) = normalize_logical_path(&loc.relative_path, Platform::Pc) {
                would_import.push(WouldImport {
                    logical_path: logical,
                    source_relative_path: loc.relative_path.clone(),
                    size: loc.size,
                    role,
                    language: loc.layout.strip_prefix("lang-").map(str::to_string),
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
        edition_guess: EditionGuess {
            edition,
            confidence,
            basis,
            verified: false,
        },
        language_guess,
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
        notes,
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
}
