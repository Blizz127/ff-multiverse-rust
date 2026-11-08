//! Finding the owner's FF7 field archive (`flevel.lgp`) and the provenance it is presented
//! with. The one resolver the server and every client share.
//!
//! The area fingerprint binds the provenance (edition, region, revision), so a client that
//! resolved a different library set than the server's would be refused at join. Both binaries
//! therefore call the same function instead of each keeping its own copy of the rules. Nothing
//! here opens or decodes the archive: that is `fmv-ff7`'s job, and this crate does not depend
//! on it.
//!
//! Two sources:
//!
//! * [`Ff7Source::Library`]: the content-addressed library `fmv-import` wrote under a state
//!   directory. The set is the one named explicitly, or the only FF7 PC set; the blob is the
//!   set's `data/lang-<language>/field/flevel.lgp` (Steam layout), else `data/field/flevel.lgp`
//!   (classic layout), else the only `flevel.lgp` anywhere in the set. The set's provenance is
//!   returned unchanged (install path included: it stays on this machine, the wire carries
//!   `EditionProvenance::summary()`).
//! * [`Ff7Source::Flevel`]: a `flevel.lgp` read directly and read-only. There is no manifest to
//!   say what edition it is, so the operator may supply the labels (they default to
//!   `unknown`). They are part of the fingerprint: two peers that read the same bytes but label
//!   them differently cannot share an instance, which is the safe direction.
//!
//! Everything here is read-only. Local paths appear in errors shown on this machine only.

use std::path::{Path, PathBuf};

use fmv_core::{EditionProvenance, GameId, Platform, SourceRef};

use crate::error::LibraryError;
use crate::library::Library;
use crate::manifest::Manifest;
use crate::paths::resolve_state_dir;

const UNKNOWN: &str = "unknown";

/// Edition labels for a `flevel.lgp` that was not imported through the library.
///
/// They stand in for the manifest's provenance, and the area fingerprint binds `edition`,
/// `region` and `revision` (not `language`). To share an instance with a peer that uses a
/// library set, give the same three labels as that set (`fmv-import list` shows them).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ff7Labels {
    pub edition: String,
    pub region: String,
    pub language: String,
    pub revision: String,
}

impl Default for Ff7Labels {
    fn default() -> Self {
        let unknown = || UNKNOWN.to_string();
        Self {
            edition: unknown(),
            region: unknown(),
            language: unknown(),
            revision: unknown(),
        }
    }
}

/// Where the FF7 field archive comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ff7Source {
    /// An imported library. `state_dir` of `None` means the default (`$FMV_STATE_DIR`, else the
    /// user data directory); `set` of `None` means the only FF7 PC set.
    Library {
        state_dir: Option<PathBuf>,
        set: Option<String>,
    },
    /// A `flevel.lgp` opened directly (read-only), with operator-supplied labels.
    Flevel { path: PathBuf, labels: Ff7Labels },
}

#[derive(Debug, thiserror::Error)]
pub enum Ff7ResolveError {
    #[error("no owned-content library at {dir}: needs owned FF7 content imported via fmv-import (for example `fmv-import import-pc <FF7 install> --state-dir {dir}`)")]
    NoLibrary { dir: String },
    #[error("cannot read the owned-content library: {0}")]
    Library(#[from] LibraryError),
    #[error("the library has no FF7 PC set: needs owned FF7 content imported via fmv-import")]
    NoFf7Set,
    #[error("the library holds several FF7 PC sets ({0}); choose one with --ff7-set")]
    AmbiguousSet(String),
    #[error("library set {set:?} cannot be used for FF7 fields: {why}")]
    UnusableSet { set: String, why: String },
    #[error("library set {0:?} has no flevel.lgp (the FF7 field archive): import it again from a complete install")]
    NoFlevel(String),
}

/// A `flevel.lgp` found on this machine and the provenance it is to be presented with.
#[derive(Debug, Clone)]
pub struct ResolvedFlevel {
    pub path: PathBuf,
    /// Unchanged from the library manifest (or built from the operator's labels), install path
    /// included. Never put it on the wire: send `provenance.summary()`.
    pub provenance: EditionProvenance,
    /// Path-free description for logs ("library set <id>" / "direct flevel.lgp").
    pub origin: String,
}

/// A set the FF7 PC adapter can read: FF7, and not PlayStation media.
fn usable_for_fields(m: &Manifest) -> Result<(), String> {
    if m.provenance.game != GameId::Ff7 {
        return Err(format!("it holds {:?} content", m.provenance.game));
    }
    if m.provenance.platform == Platform::Ps1 {
        return Err("it is PlayStation media; only the PC field archive layout is supported".into());
    }
    Ok(())
}

/// The project's FIXTURE predicate (synthetic content is never real FF7).
fn is_fixture(m: &Manifest) -> bool {
    m.provenance.game.is_fixture()
        || m.provenance.platform == Platform::Synthetic
        || m.provenance
            .sources
            .iter()
            .any(|s| matches!(s, SourceRef::Fixture))
}

/// Which set to read. An explicit id is taken as given (a synthetic FF7-format set is allowed
/// then, and the server labels it FIXTURE); without one, only real FF7 PC sets count, and there
/// must be exactly one.
fn pick_set(lib: &Library, requested: Option<&str>) -> Result<(String, Manifest), Ff7ResolveError> {
    if let Some(id) = requested {
        let manifest = lib.load_manifest(id)?;
        usable_for_fields(&manifest).map_err(|why| Ff7ResolveError::UnusableSet {
            set: id.to_string(),
            why,
        })?;
        return Ok((id.to_string(), manifest));
    }
    let mut found: Vec<(String, Manifest)> = Vec::new();
    for id in lib.list_sets()? {
        // A set whose manifest cannot be read is not a candidate; naming it explicitly shows why.
        let Ok(manifest) = lib.load_manifest(&id) else {
            continue;
        };
        if usable_for_fields(&manifest).is_ok() && !is_fixture(&manifest) {
            found.push((id, manifest));
        }
    }
    if found.len() > 1 {
        let ids = found.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>();
        return Err(Ff7ResolveError::AmbiguousSet(ids.join(", ")));
    }
    found.into_iter().next().ok_or(Ff7ResolveError::NoFf7Set)
}

/// The logical path of the set's field archive: the language directory of the Steam layout
/// first, then the classic layout, then a single `flevel.lgp` anywhere else.
fn flevel_logical_path(m: &Manifest) -> Option<String> {
    let language = m.provenance.language.to_ascii_lowercase();
    let preferred = [
        format!("data/lang-{language}/field/flevel.lgp"),
        "data/field/flevel.lgp".to_string(),
    ];
    if let Some(path) = preferred.into_iter().find(|p| m.find(p).is_some()) {
        return Some(path);
    }
    let mut any = m
        .entries
        .iter()
        .filter(|e| e.logical_path.rsplit('/').next() == Some("flevel.lgp"));
    match (any.next(), any.next()) {
        (Some(only), None) => Some(only.logical_path.clone()),
        _ => None,
    }
}

/// Find the `flevel.lgp` of an imported library set and its provenance. `state_dir` of `None`
/// is the default state directory; `set` of `None` is the only FF7 PC set. Opens the library
/// read-only (nothing is created or written).
pub fn resolve_library_flevel(
    state_dir: Option<&Path>,
    set: Option<&str>,
) -> Result<ResolvedFlevel, Ff7ResolveError> {
    let dir = resolve_state_dir(state_dir)?;
    let lib = Library::open_existing(&dir).map_err(|e| match e {
        LibraryError::NoLibrary(_) => Ff7ResolveError::NoLibrary {
            dir: dir.display().to_string(),
        },
        other => Ff7ResolveError::Library(other),
    })?;
    let (set_id, manifest) = pick_set(&lib, set)?;
    let logical = flevel_logical_path(&manifest).ok_or_else(|| Ff7ResolveError::NoFlevel(set_id.clone()))?;
    // `get` checks the blob's size against the manifest (the full content check is
    // `Library::verify_entry`; the area fingerprint hashes the archive entry anyway).
    let path = lib
        .get(&set_id, &logical)?
        .ok_or_else(|| Ff7ResolveError::NoFlevel(set_id.clone()))?;
    Ok(ResolvedFlevel {
        path,
        provenance: manifest.provenance,
        origin: format!("library set {set_id}"),
    })
}

/// Find the `flevel.lgp` and its provenance without opening the archive.
pub fn resolve_flevel(source: &Ff7Source) -> Result<ResolvedFlevel, Ff7ResolveError> {
    match source {
        Ff7Source::Library { state_dir, set } => resolve_library_flevel(state_dir.as_deref(), set.as_deref()),
        Ff7Source::Flevel { path, labels } => {
            // A flevel.lgp is the PC archive layout, so the platform is PC; everything else the
            // operator knows is in the labels.
            let provenance = EditionProvenance {
                game: GameId::Ff7,
                platform: Platform::Pc,
                edition: labels.edition.clone(),
                region: labels.region.clone(),
                language: labels.language.clone(),
                revision: labels.revision.clone(),
                sources: vec![SourceRef::Install {
                    path: path.display().to_string(),
                }],
                modded: false,
            };
            Ok(ResolvedFlevel {
                path: path.clone(),
                provenance,
                origin: "direct flevel.lgp".to_string(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::SetHandle;
    use std::fs;

    fn provenance(edition: &str, language: &str) -> EditionProvenance {
        EditionProvenance {
            game: GameId::Ff7,
            platform: Platform::Pc,
            edition: edition.into(),
            region: "unknown".into(),
            language: language.into(),
            revision: "rev1".into(),
            sources: vec![SourceRef::Install {
                path: "/somewhere/private".into(),
            }],
            modded: false,
        }
    }

    /// Import `files` (logical path, bytes) as a set with `p` into `lib`; returns the set id.
    fn make_set(lib: &Library, p: EditionProvenance, files: &[(&str, &[u8])], scratch: &Path) -> String {
        let handle = SetHandle::new(p);
        for (i, (logical, bytes)) in files.iter().enumerate() {
            let src = scratch.join(format!("src-{i}"));
            fs::write(&src, bytes).unwrap();
            lib.import_file(
                &handle,
                logical,
                &src,
                SourceRef::Install {
                    path: "/somewhere/private".into(),
                },
                1,
            )
            .unwrap();
        }
        handle.set_id
    }

    fn lib_source(dir: &Path, set: Option<&str>) -> Ff7Source {
        Ff7Source::Library {
            state_dir: Some(dir.to_path_buf()),
            set: set.map(str::to_string),
        }
    }

    #[test]
    fn the_steam_language_layout_wins_over_the_classic_one_and_blobs_are_found() {
        let state = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let lib = Library::open(state.path()).unwrap();
        let id = make_set(
            &lib,
            provenance("Steam", "en"),
            &[
                ("data/field/flevel.lgp", b"classic"),
                ("data/lang-en/field/flevel.lgp", b"steam-language"),
            ],
            scratch.path(),
        );
        let r = resolve_flevel(&lib_source(state.path(), None)).unwrap();
        assert_eq!(fs::read(&r.path).unwrap(), b"steam-language");
        assert_eq!(r.origin, format!("library set {id}"));
        assert_eq!(r.provenance.edition, "Steam");
        // The provenance goes through unchanged, install path included (it never reaches the wire).
        assert!(r
            .provenance
            .sources
            .iter()
            .any(|s| matches!(s, SourceRef::Install { .. })));
        assert!(!format!("{:?}", r.provenance.summary()).contains("private"));
    }

    #[test]
    fn the_classic_layout_and_a_lone_flevel_elsewhere_are_found() {
        let scratch = tempfile::tempdir().unwrap();
        for (logical, body) in [
            ("data/field/flevel.lgp", &b"classic"[..]),
            ("data/odd/place/flevel.lgp", &b"odd"[..]),
        ] {
            let state = tempfile::tempdir().unwrap();
            let lib = Library::open(state.path()).unwrap();
            make_set(&lib, provenance("PC 1998", "en"), &[(logical, body)], scratch.path());
            let r = resolve_flevel(&lib_source(state.path(), None)).unwrap();
            assert_eq!(fs::read(&r.path).unwrap(), body, "{logical}");
        }
    }

    #[test]
    fn several_flevels_in_one_set_without_a_preferred_location_are_not_guessed() {
        let state = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let lib = Library::open(state.path()).unwrap();
        make_set(
            &lib,
            provenance("PC 1998", "en"),
            &[("data/a/flevel.lgp", b"a"), ("data/b/flevel.lgp", b"b")],
            scratch.path(),
        );
        let err = resolve_flevel(&lib_source(state.path(), None)).unwrap_err();
        assert!(matches!(err, Ff7ResolveError::NoFlevel(_)), "{err}");
    }

    #[test]
    fn missing_libraries_sets_and_archives_say_what_to_do() {
        // No library at all: the message points at fmv-import.
        let empty = tempfile::tempdir().unwrap();
        let err = resolve_flevel(&lib_source(empty.path(), None)).unwrap_err();
        assert!(matches!(err, Ff7ResolveError::NoLibrary { .. }), "{err}");
        assert!(
            err.to_string().contains("needs owned FF7 content imported via fmv-import"),
            "{err}"
        );
        // Resolving never creates anything.
        assert!(fs::read_dir(empty.path()).unwrap().next().is_none());

        // A library without any FF7 set.
        let state = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let lib = Library::open(state.path()).unwrap();
        let err = resolve_flevel(&lib_source(state.path(), None)).unwrap_err();
        assert!(matches!(err, Ff7ResolveError::NoFf7Set), "{err}");

        // An FF7 set that never got its field archive.
        let id = make_set(
            &lib,
            provenance("PC 1998", "en"),
            &[("data/field/char.lgp", b"x")],
            scratch.path(),
        );
        let err = resolve_flevel(&lib_source(state.path(), None)).unwrap_err();
        assert!(matches!(err, Ff7ResolveError::NoFlevel(ref s) if *s == id), "{err}");

        // An explicit set id that does not exist, or is not a set id at all.
        assert!(matches!(
            resolve_flevel(&lib_source(state.path(), Some("ff7-pc-nope-00000000"))).unwrap_err(),
            Ff7ResolveError::Library(LibraryError::SetNotFound(_))
        ));
        assert!(resolve_flevel(&lib_source(state.path(), Some("../escape"))).is_err());
    }

    #[test]
    fn several_ff7_sets_need_an_explicit_choice_and_fixture_sets_are_never_picked_silently() {
        let state = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let lib = Library::open(state.path()).unwrap();
        let a = make_set(
            &lib,
            provenance("Steam", "en"),
            &[("data/field/flevel.lgp", b"a")],
            scratch.path(),
        );
        let synthetic = EditionProvenance {
            platform: Platform::Synthetic,
            sources: vec![SourceRef::Fixture],
            edition: "synthetic".into(),
            ..provenance("x", "none")
        };
        let fixture = make_set(
            &lib,
            synthetic,
            &[("data/field/flevel.lgp", b"fixture")],
            scratch.path(),
        );

        // The synthetic set is ignored without an explicit id, so the real one is the only candidate.
        let r = resolve_flevel(&lib_source(state.path(), None)).unwrap();
        assert_eq!(fs::read(&r.path).unwrap(), b"a");
        // Naming it explicitly works (the server then labels the area FIXTURE).
        let r = resolve_flevel(&lib_source(state.path(), Some(&fixture))).unwrap();
        assert_eq!(fs::read(&r.path).unwrap(), b"fixture");

        // A second real set makes the choice ambiguous, and the error lists both.
        let b = make_set(
            &lib,
            provenance("PC 1998", "en"),
            &[("data/field/flevel.lgp", b"b")],
            scratch.path(),
        );
        let err = resolve_flevel(&lib_source(state.path(), None)).unwrap_err();
        assert!(matches!(err, Ff7ResolveError::AmbiguousSet(_)), "{err}");
        assert!(err.to_string().contains(&a) && err.to_string().contains(&b), "{err}");
        assert!(err.to_string().contains("--ff7-set"), "{err}");
        let r = resolve_flevel(&lib_source(state.path(), Some(&b))).unwrap();
        assert_eq!(fs::read(&r.path).unwrap(), b"b");
    }

    #[test]
    fn non_ff7_and_playstation_sets_are_refused_even_when_named() {
        let state = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let lib = Library::open(state.path()).unwrap();
        let ps1 = EditionProvenance {
            platform: Platform::Ps1,
            ..provenance("PS1 original", "en")
        };
        let id = make_set(&lib, ps1, &[("data/field/flevel.lgp", b"x")], scratch.path());
        let err = resolve_flevel(&lib_source(state.path(), Some(&id))).unwrap_err();
        assert!(matches!(err, Ff7ResolveError::UnusableSet { .. }), "{err}");
        assert!(err.to_string().contains("PlayStation"), "{err}");
        // ...and it is not a candidate for the automatic choice either.
        assert!(matches!(
            resolve_flevel(&lib_source(state.path(), None)).unwrap_err(),
            Ff7ResolveError::NoFf7Set
        ));

        let ff8 = EditionProvenance {
            game: GameId::Ff8,
            ..provenance("FF8", "en")
        };
        let id = make_set(&lib, ff8, &[("data/field/flevel.lgp", b"x")], scratch.path());
        assert!(matches!(
            resolve_flevel(&lib_source(state.path(), Some(&id))).unwrap_err(),
            Ff7ResolveError::UnusableSet { .. }
        ));
    }

    #[test]
    fn a_direct_flevel_carries_the_operators_labels_and_is_not_synthetic() {
        let labels = Ff7Labels {
            edition: "Steam".into(),
            region: "unknown".into(),
            language: "en".into(),
            revision: "rev1".into(),
        };
        let r = resolve_flevel(&Ff7Source::Flevel {
            path: "/x/flevel.lgp".into(),
            labels: labels.clone(),
        })
        .unwrap();
        assert_eq!(r.path, Path::new("/x/flevel.lgp"));
        assert_eq!(r.provenance.game, GameId::Ff7);
        assert_eq!(r.provenance.platform, Platform::Pc);
        assert_eq!((r.provenance.edition.as_str(), r.provenance.revision.as_str()), ("Steam", "rev1"));
        assert_eq!(r.origin, "direct flevel.lgp");

        // Defaults are `unknown`, which never matches a labelled library set.
        let d = resolve_flevel(&Ff7Source::Flevel {
            path: "/x".into(),
            labels: Ff7Labels::default(),
        })
        .unwrap();
        assert!(!d.provenance.compatible_with(&r.provenance));
        assert_eq!(d.provenance.edition, "unknown");
    }

    #[test]
    fn a_library_set_and_a_direct_flevel_with_its_labels_are_compatible_peers() {
        let state = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let lib = Library::open(state.path()).unwrap();
        make_set(
            &lib,
            provenance("Steam", "en"),
            &[("data/field/flevel.lgp", b"a")],
            scratch.path(),
        );
        let from_library = resolve_flevel(&lib_source(state.path(), None)).unwrap();
        let direct = resolve_flevel(&Ff7Source::Flevel {
            path: "/anywhere/flevel.lgp".into(),
            labels: Ff7Labels {
                edition: "Steam".into(),
                region: "unknown".into(),
                language: "en".into(),
                revision: "rev1".into(),
            },
        })
        .unwrap();
        assert!(from_library.provenance.compatible_with(&direct.provenance));
    }
}
