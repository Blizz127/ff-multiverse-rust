//! `import-pc`: import the needed files of an FF7 PC install into one library set.
//!
//! One import = one language. The scan lists every candidate file tagged with its language;
//! [`ScanReport::select`](crate::scan::ScanReport::select) keeps the language-neutral files plus
//! the files of exactly one language, so a multi-language install never ends up as a single set
//! labelled with one (or an unknown) language. Adapters reading the set should look in
//! `data/lang-<set language>/` first and fall back to the classic `data/` layout.
//!
//! The directory given is the folder that holds `data/`, or the install root of the Steam Edition
//! (or its `ff7` folder): the scan finds `ff7/workingdir/data` itself. Library paths are always
//! relative to the folder that holds `data/` (`data/field/flevel.lgp`), so every spelling of one
//! install imports the same set.
//!
//! Symbolic links: a file that is a link (or sits behind one) is followed only while it stays
//! inside the directory given, resolved to its real path; one that leads out of it is refused
//! (the scan leaves it out and says so, and the import checks again, on the real path, right
//! before it reads the file). The logical path of a file is always the scan's, never the
//! resolved one, so a link inside the tree changes nothing about what the set holds. Labels not given on the command line come from the scan:
//! the Steam Edition is `Steam Edition (appid 3837340)` / region `unknown` / revision
//! `build <id>`, the exact strings `--edition` and `--revision` took for the sets that exist.
//!
//! Audio: the install's music and sound files (`data/music_ogg/*.ogg`, `data/music/*.wav` and
//! `music.idx`, `data/sound/audio.dat` and `audio.fmt`, see [`crate::scan`]) are language-neutral
//! files of role `audio` and are imported with the rest, by the same machinery and the same checks
//! (hash, stamps before and after, a second read hashed again, a state directory that may not be
//! inside the install). Audio alone is not an import: without a `flevel.lgp` the import is
//! refused as ever. Importing an install again with the same labels (the same set id) ADDS what
//! it has now to the set that is there and leaves every entry that exists as it is; audio never
//! counts for what the manifest records about its own identification (`identification_for`), so
//! the set says the same about itself with or without it. Other labels make another set.

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
use crate::scan::{
    scan_pc, EditionGuess, ScanError, ScanReport, WouldImport, AUDIO_WARNING_PREFIX,
    LANGUAGE_DEFAULT_PREFIX,
};

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

/// The real file behind `rel` (a path relative to the install directory `install`, which is
/// canonical already): every link resolved, and refused when the real path is not inside the
/// install. What is stamped, copied and hashed again is this real path.
pub(super) fn resolve_in_install(install: &Path, rel: &str) -> Result<PathBuf, CommandError> {
    let joined = install.join(rel);
    let real = fs::canonicalize(&joined).map_err(|source| ScanError::Io {
        path: joined.clone(),
        source,
    })?;
    if !real.starts_with(install) {
        return Err(CommandError::SymlinkOutsideInstall {
            path: rel.to_string(),
            target: real,
        });
    }
    Ok(real)
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
    /// The folder that holds `data/`, or the install root of the Steam Edition (or its `ff7`
    /// folder).
    pub dir: PathBuf,
    pub state_dir: Option<PathBuf>,
    /// Overrides the scan's edition label.
    pub edition: Option<String>,
    pub region: Option<String>,
    /// Language to import. Required when the install holds several and says nothing about which
    /// one is meant (the Steam Edition defaults to `en`, with a warning); must match one of them
    /// (case-insensitively). On an install without any language marker it is just a label.
    pub language: Option<String>,
    /// Overrides the scan's revision label (`build <id>` for a Steam Edition whose Steam
    /// appmanifest was found, else `unknown`).
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
    #[cfg(test)]
    seam::after_scan(&args.dir);

    // Every refusal up to the library being opened happens before anything is written.
    if !report.would_import.iter().any(|w| is_flevel(&w.logical_path)) {
        // Only links that took a game file with them: a refused link to a piece of audio is no
        // reason a flevel.lgp is missing (audio alone is not an import either way).
        let links: Vec<&str> = report
            .refused_links
            .iter()
            .filter(|l| !l.audio)
            .take(4)
            .map(|l| l.relative_path.as_str())
            .collect();
        if !links.is_empty() {
            return Err(CommandError::NoFlevelOnlyLinks {
                dir: args.dir.display().to_string(),
                links: links.join(", "),
            });
        }
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
    // the directory given, and the Steam Edition install root it belongs to (which can sit above
    // it): Steam manages that folder, nothing of ours may be written there either
    let mut protected = vec![install.clone()];
    protected.extend(report.steam_install_root.iter().cloned());
    for guarded in [state_dir.clone(), canonical_lenient(&state_dir.join("library"))] {
        if let Some(inside) = protected.iter().find(|p| guarded.starts_with(p)) {
            return Err(CommandError::StateInsideSource {
                state: requested_state,
                install: inside.clone(),
            });
        }
    }

    // The REAL files (links resolved, confined to the install): they are what is stamped, copied
    // and hashed again; the logical paths stay the scan's. Everything that can refuse an import
    // from the install alone happens here, before the library is opened: a refusal creates
    // nothing.
    let dir_mtime_before = fs::metadata(&install).and_then(|m| m.modified()).ok();
    let mut sources: Vec<(PathBuf, SourceStamp)> = Vec::new();
    for w in &chosen {
        let path = resolve_in_install(&install, &w.source_relative_path)?;
        let stamp = SourceStamp::capture(&path)?;
        sources.push((path, stamp));
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
        // not given: the scan's (`build <id>` from Steam's appmanifest), else unknown
        revision: args
            .revision
            .clone()
            .or_else(|| report.edition_guess.revision.clone())
            .unwrap_or_else(|| "unknown".to_string()),
        sources: vec![source_ref.clone()],
        modded: report.modded,
    };
    // The classic `data/...` layout is taken as language-neutral and kept next to the chosen
    // language's own files. On an install that also has language directories that is an
    // assumption (the Steam layout is uncertain [verify]); the manifest says so. Audio does not
    // count (like the executables): it has no language variants to be mistaken for, and adding it
    // to a set later must not change what the set says about itself.
    let neutral_data_beside_languages = !report.languages.is_empty()
        && chosen
            .iter()
            .any(|w| w.language.is_none() && w.role != "identify" && w.role != "audio");
    let identification = identification_for(
        args,
        &report,
        &chosen,
        &language,
        neutral_data_beside_languages,
    );
    let handle =
        SetHandle::new(provenance.clone()).with_identification(identification.clone());

    let mut batch = lib.begin_import(&handle, now)?;
    let batch_locked = batch.is_locked();
    let mut files = Vec::new();
    let mut hashes = Vec::new();
    let mut totals = Totals::default();
    for (w, (path, _)) in chosen.iter().zip(&sources) {
        let outcome =
            batch.add_file_within(&w.logical_path, path, &install, source_ref.clone())?;
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
    if !batch_locked {
        warnings.push("this filesystem does not support advisory file locks, so nothing could stop a second import into this set from running at the same time: do not run two imports at once (a collision can lose entries, which importing again restores)".to_string());
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
    report: &ScanReport,
    chosen: &[WouldImport],
    language: &str,
    neutral_data_beside_languages: bool,
) -> Identification {
    let given = |v: &Option<String>| v.as_ref().is_some_and(|s| !s.trim().is_empty());
    let guess = &report.edition_guess;
    // Steam's appmanifest decided a label that was not given on the command line
    let edition_from_steam = guess.steam.is_some() && !given(&args.edition);
    let revision_from_steam = guess.revision.is_some() && !given(&args.revision);
    let mut caveats = Vec::new();
    caveats.push(match &guess.steam {
        Some(s) if edition_from_steam => format!(
            "edition was identified from the install files and Steam's {} (appid {}); it was not compared with hashes of known releases",
            s.manifest_file, s.appid
        ),
        _ => "edition is a guess from file presence; it was not compared with hashes of known releases"
            .to_string(),
    });
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
    } else if report.language_defaulted && report.language_guess.as_deref() == Some(language) {
        format!(
            "language defaulted to {language}: the install holds several languages and none was chosen (--language)"
        )
    } else {
        "language is inferred from the executable / language directory names, or unknown"
            .to_string()
    });
    caveats.push(if given(&args.revision) {
        "revision was given by the user (--revision) and is not checked".to_string()
    } else if let Some(rev) = &guess.revision {
        format!("revision was read from Steam's appmanifest ({rev}): the depot build, not compared with hashes of known releases")
    } else {
        "revision is unknown: it cannot be told from the files".to_string()
    });
    if neutral_data_beside_languages {
        caveats.push("files outside data/lang-<xx>/ (the classic layout) were taken as language-neutral and kept beside the chosen language's own files: that is an assumption, the layout of installs with language directories is uncertain [verify]".to_string());
    }
    // the field archive's language comes from file names alone: flevel.lgp = English and
    // fflevel / gflevel / sflevel / jfleve.lgp = French / German / Spanish / Japanese
    if let Some(f) = chosen
        .iter()
        .find(|w| w.language_inferred && is_flevel(&w.logical_path))
    {
        let file = f
            .source_relative_path
            .rsplit('/')
            .next()
            .unwrap_or(&f.source_relative_path);
        let stored = if f.variant_of.is_some() {
            format!(
                ", stored as {} so that readers find it as that language's flevel.lgp",
                f.logical_path
            )
        } else {
            String::new()
        };
        caveats.push(format!(
            "the field archive ({file}) was assigned to language {} from file names alone (flevel.lgp = English; fflevel, gflevel, sflevel, jfleve.lgp = French, German, Spanish, Japanese) [verify]{stored}",
            f.language.as_deref().unwrap_or("unknown")
        ));
    }
    // the first warning about the files themselves (the language default has its own caveat, and
    // a warning about audio says nothing about how far the labels can be trusted)
    if let Some(first) = report
        .warnings
        .iter()
        .find(|w| !w.starts_with(LANGUAGE_DEFAULT_PREFIX) && !w.starts_with(AUDIO_WARNING_PREFIX))
    {
        caveats.push(first.clone());
    }
    let method = if edition_from_steam || revision_from_steam {
        "pc-steam-appmanifest"
    } else {
        "pc-file-presence"
    };
    Identification::unverified(method, caveats)
}

/// Test seam: lets a test change the install between the scan and the import's own checks (a
/// file swapped for a link, say), which no file-system fixture can do from outside.
#[cfg(test)]
pub(super) mod seam {
    use std::cell::RefCell;
    use std::path::Path;

    type Hook = Box<dyn Fn(&Path)>;

    thread_local! {
        pub(in crate::commands) static AFTER_SCAN: RefCell<Option<Hook>> = const { RefCell::new(None) };
    }

    pub(super) fn after_scan(dir: &Path) {
        AFTER_SCAN.with(|h| {
            if let Some(f) = h.borrow().as_ref() {
                f(dir);
            }
        });
    }
}
