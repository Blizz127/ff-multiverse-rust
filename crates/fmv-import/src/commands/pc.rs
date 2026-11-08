//! `import-pc`: import the needed files of an FF7 PC install into one library set.
//!
//! One import = one language. The scan lists every candidate file tagged with its language;
//! [`ScanReport::select`](crate::scan::ScanReport::select) keeps the language-neutral files plus
//! the files of exactly one language, so a multi-language install never ends up as a single set
//! labelled with one (or an unknown) language. Adapters reading the set should look in
//! `data/lang-<set language>/` first and fall back to the classic `data/` layout.

use std::fs;
use std::path::{Component, Path, PathBuf};

use fmv_core::{EditionProvenance, GameId, Platform, ProvenanceSummary, SourceRef};
use fmv_library::{
    hash_file, resolve_state_dir, Identification, Library, LibraryError, SetHandle, SourceStamp,
};
use serde::Serialize;

use super::{
    imported, importer_version, CommandError, ImportedFile, SourceEvidence, SourceSummary, Totals,
};
use crate::scan::{scan_pc, EditionGuess, ScanError, WouldImport};

/// Absolute, symlink-resolved form of `p` even when `p` (or its tail) does not exist yet.
///
/// Components are resolved the way the operating system resolves them, left to right: a symlink
/// is followed BEFORE the `..` after it is applied. (Folding `..` away lexically first, as an
/// earlier version did, makes `<symlink>/../x` mean something else than it does to the
/// filesystem and lets a state directory slip into the install it must stay out of.) Components
/// that do not exist yet are kept as they are; a `..` that cancels one of them removes it again,
/// which is what `create_dir_all` ends up doing with such a path.
pub(super) fn canonical_lenient(p: &Path) -> PathBuf {
    let abs = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
    let mut cur = PathBuf::new();
    // how many trailing components of `cur` do not exist (yet)
    let mut missing = 0usize;
    for comp in abs.components() {
        match comp {
            Component::Prefix(_) | Component::RootDir => cur.push(comp.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                // `cur` is either fully resolved (no symlinks left in it) or ends in components
                // that do not exist: in both cases dropping the last component is exact
                cur.pop();
                missing = missing.saturating_sub(1);
            }
            Component::Normal(name) => {
                cur.push(name);
                if missing > 0 {
                    missing += 1;
                } else {
                    match fs::canonicalize(&cur) {
                        Ok(real) => cur = real,
                        Err(_) => missing = 1,
                    }
                }
            }
        }
    }
    cur
}

/// Err unless every source still has its stamp (size, mtime, identity) AND, read a second time,
/// hashes to what the copy saw. The stamp alone is fooled by a file rewritten in place with the
/// same size and a restored mtime; the re-read is not.
pub(super) fn recheck_sources(
    sources: &[(PathBuf, SourceStamp)],
    copied_hashes: &[String],
) -> Result<(), LibraryError> {
    for ((path, stamp), copied) in sources.iter().zip(copied_hashes) {
        stamp.verify(path)?;
        if hash_file(path)? != *copied {
            return Err(LibraryError::SourceChanged(path.clone()));
        }
    }
    Ok(())
}

fn is_flevel(logical_path: &str) -> bool {
    logical_path.rsplit('/').next() == Some("flevel.lgp")
}

#[derive(Debug, Clone)]
pub struct ImportPcArgs {
    pub dir: PathBuf,
    pub state_dir: Option<PathBuf>,
    pub edition: Option<String>,
    pub region: Option<String>,
    /// Language to import. Required when the install holds several; must match one of them
    /// (case-insensitively). On an install without any language marker it is just a label.
    pub language: Option<String>,
    pub revision: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ImportPcResult {
    pub command: &'static str,
    pub set_id: String,
    pub state_dir: String,
    /// The path-free provenance of the set (what other players could be shown).
    pub provenance: ProvenanceSummary,
    /// Where it came from, without the install path.
    pub source: SourceSummary,
    /// How far the labels above can be trusted. Also stored in the manifest.
    pub identification: Identification,
    pub edition_guess: EditionGuess,
    /// The language this set holds (one per set).
    pub language: String,
    /// Other languages of the install that are NOT part of this import.
    pub languages_left_out: Vec<String>,
    pub files: Vec<ImportedFile>,
    pub totals: Totals,
    /// Every source file had the same size, mtime and identity before and after the import AND
    /// hashed to the same blake3 when read a second time (see `source_evidence`).
    pub source_files_unchanged: bool,
    pub source_evidence: SourceEvidence,
    /// The install directory's own mtime did not move (informational: another process could
    /// legitimately touch it).
    pub source_dir_mtime_unchanged: bool,
    pub warnings: Vec<String>,
    pub notes: Vec<&'static str>,
}

pub fn import_pc(args: &ImportPcArgs, now: u64) -> Result<ImportPcResult, CommandError> {
    let report = scan_pc(&args.dir)?;

    // Every refusal up to the library being opened happens before anything is written.
    if !report.would_import.iter().any(|w| is_flevel(&w.logical_path)) {
        return Err(CommandError::NoFlevel(args.dir.display().to_string()));
    }
    let selection = report.select(args.language.as_deref())?;
    if !selection.has_flevel() {
        let mut have: Vec<String> = report
            .would_import
            .iter()
            .filter(|w| is_flevel(&w.logical_path))
            .map(|w| w.language.clone().unwrap_or_else(|| "neutral".to_string()))
            .collect();
        have.sort();
        have.dedup();
        return Err(CommandError::NoFlevelForLanguage {
            language: selection.language.clone(),
            have: have.join(", "),
        });
    }
    let language = selection.language.clone();
    let languages_left_out = selection.left_out.clone();
    let chosen: Vec<WouldImport> = selection.files.iter().map(|w| (*w).clone()).collect();

    let install = fs::canonicalize(&args.dir).map_err(|source| ScanError::Io {
        path: args.dir.clone(),
        source,
    })?;
    // The state directory is resolved the way the filesystem resolves it, and the library is
    // opened at that resolved path: the guard and the writes cannot disagree about where it is.
    let requested_state = resolve_state_dir(args.state_dir.as_deref())?;
    let state_dir = canonical_lenient(&requested_state);
    for guarded in [state_dir.clone(), canonical_lenient(&state_dir.join("library"))] {
        if guarded.starts_with(&install) {
            return Err(CommandError::StateInsideSource {
                state: requested_state,
                install,
            });
        }
    }

    let lib = Library::open(&state_dir)?.with_importer_version(importer_version());
    let source_ref = SourceRef::Install {
        path: install.display().to_string(),
    };
    let provenance = EditionProvenance {
        game: GameId::Ff7,
        platform: Platform::Pc,
        edition: args
            .edition
            .clone()
            .unwrap_or_else(|| report.edition_guess.edition.clone()),
        region: args.region.clone().unwrap_or_else(|| "unknown".to_string()),
        language: language.clone(),
        revision: args
            .revision
            .clone()
            .unwrap_or_else(|| "unknown".to_string()),
        sources: vec![source_ref.clone()],
        modded: report.modded,
    };
    // The classic `data/...` layout is taken as language-neutral and kept next to the chosen
    // language's own files. On an install that also has language directories that is an
    // assumption (the Steam layout is uncertain [verify]); the manifest says so.
    let neutral_data_beside_languages = !report.languages.is_empty()
        && chosen
            .iter()
            .any(|w| w.language.is_none() && w.role != "identify");
    let identification = identification_for(args, &report.warnings, neutral_data_beside_languages);
    let handle =
        SetHandle::new(provenance.clone()).with_identification(identification.clone());

    let dir_mtime_before = fs::metadata(&install).and_then(|m| m.modified()).ok();
    let mut sources: Vec<(PathBuf, SourceStamp)> = Vec::new();
    for w in &chosen {
        let path = install.join(&w.source_relative_path);
        let stamp = SourceStamp::capture(&path)?;
        sources.push((path, stamp));
    }

    let mut batch = lib.begin_import(&handle, now)?;
    let mut files = Vec::new();
    let mut hashes = Vec::new();
    let mut totals = Totals::default();
    for (w, (path, _)) in chosen.iter().zip(&sources) {
        let outcome = batch.add_file(&w.logical_path, path, source_ref.clone())?;
        totals.add(&outcome);
        hashes.push(outcome.blake3.clone());
        files.push(imported(&outcome));
    }
    // the sources must be exactly as they were before anything is persisted
    recheck_sources(&sources, &hashes)?;
    batch.commit()?;
    let dir_mtime_after = fs::metadata(&install).and_then(|m| m.modified()).ok();

    let mut warnings = Vec::new();
    if lib.stale_parts_removed() > 0 {
        warnings.push(format!(
            "removed {} leftover temporary file(s) of an interrupted earlier import from the library",
            lib.stale_parts_removed()
        ));
    }
    if totals.blobs_repaired > 0 {
        warnings.push(format!(
            "{} damaged blob(s) already in the library were replaced from this install",
            totals.blobs_repaired
        ));
    }
    if !languages_left_out.is_empty() {
        warnings.push(format!(
            "other languages in this install were NOT imported ({}): run again with --language <code> to import one of them as a set of its own",
            languages_left_out.join(", ")
        ));
    }
    warnings.extend(report.warnings.iter().cloned());

    Ok(ImportPcResult {
        command: "import-pc",
        set_id: handle.set_id,
        state_dir: state_dir.display().to_string(),
        provenance: provenance.summary(),
        source: SourceSummary::from_source(&source_ref, false),
        identification,
        edition_guess: report.edition_guess,
        language,
        languages_left_out,
        files,
        totals,
        // a mismatch aborts the import in `recheck_sources` above, so reaching this point means
        // every check passed; the evidence below says what was compared
        source_files_unchanged: true,
        source_evidence: SourceEvidence {
            method: "size, mtime and inode compared before and after every copy; then every file read again and hashed (blake3) against the copy",
            files_checked: sources.len() as u64,
            stamps_unchanged: true,
            content_rehash_unchanged: Some(true),
        },
        source_dir_mtime_unchanged: dir_mtime_before == dir_mtime_after,
        warnings,
        notes: vec![
            "Edition, region and language are guesses or labels given on the command line: UNVERIFIED [verify]. The manifest says so (identification.verified = false).",
            "Nothing was written inside the install directory; the library lives under the state directory.",
        ],
    })
}

/// What the manifest records about how far this import's labels can be trusted. Deterministic
/// for the same inputs, so re-importing an unchanged install does not rewrite the manifest.
fn identification_for(
    args: &ImportPcArgs,
    scan_warnings: &[String],
    neutral_data_beside_languages: bool,
) -> Identification {
    let given = |v: &Option<String>| v.as_ref().is_some_and(|s| !s.trim().is_empty());
    let mut caveats = vec![
        "edition is a guess from file presence; it was not compared with hashes of known releases"
            .to_string(),
    ];
    if given(&args.edition) {
        caveats.push("edition was given by the user (--edition) and is not checked".to_string());
    }
    caveats.push(if given(&args.region) {
        "region was given by the user (--region) and is not checked".to_string()
    } else {
        "region is unknown: it cannot be told from the files".to_string()
    });
    caveats.push(if given(&args.language) {
        "language was chosen by the user (--language)".to_string()
    } else {
        "language is inferred from the executable / language directory names, or unknown"
            .to_string()
    });
    caveats.push(if given(&args.revision) {
        "revision was given by the user (--revision) and is not checked".to_string()
    } else {
        "revision is unknown: it cannot be told from the files".to_string()
    });
    if neutral_data_beside_languages {
        caveats.push("files outside data/lang-<xx>/ (the classic layout) were taken as language-neutral and kept beside the chosen language's own files: that is an assumption, the layout of installs with language directories is uncertain [verify]".to_string());
    }
    if let Some(first) = scan_warnings.first() {
        caveats.push(first.clone());
    }
    Identification::unverified("pc-file-presence", caveats)
}
