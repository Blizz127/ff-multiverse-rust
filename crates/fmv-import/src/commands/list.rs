//! `list`: the library's sets with a provenance summary.

use std::path::Path;

use fmv_core::{Platform, ProvenanceSummary, SourceRef};
use fmv_library::{resolve_state_dir, Identification, Library, LibraryError};
use serde::Serialize;

use super::{CommandError, SourceSummary};

#[derive(Debug, Serialize)]
pub struct SetListing {
    pub set_id: String,
    /// Human label of the game. Synthetic fixture content says FIXTURE here, whatever game its
    /// labels name.
    pub label: Option<String>,
    pub fixture: bool,
    /// The path-free provenance (`EditionProvenance::summary()`): game, platform, edition,
    /// region, language, revision, modded. Absent when the manifest could not be read.
    #[serde(flatten)]
    pub summary: Option<ProvenanceSummary>,
    pub files: Option<usize>,
    pub bytes: Option<u64>,
    pub imported_at: Option<u64>,
    pub importer_version: Option<String>,
    /// False unless the labels were checked against known-good hashes. A set whose manifest has
    /// no identification record counts as unverified.
    pub identification_verified: Option<bool>,
    /// How the labels were obtained and what is only a guess.
    pub identification: Option<Identification>,
    /// Where the files came from. Install paths are left out unless `--show-paths` was given.
    pub sources: Vec<SourceSummary>,
    /// Set instead of the fields above when the manifest could not be read.
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ListResult {
    pub command: &'static str,
    pub state_dir: String,
    pub library_exists: bool,
    pub sets: Vec<SetListing>,
}

pub fn list_sets(state_dir: Option<&Path>, show_paths: bool) -> Result<ListResult, CommandError> {
    let state_dir = resolve_state_dir(state_dir)?;
    let shown = state_dir.display().to_string();
    let lib = match Library::open_existing(&state_dir) {
        Ok(l) => l,
        Err(LibraryError::NoLibrary(_)) => {
            return Ok(ListResult {
                command: "list",
                state_dir: shown,
                library_exists: false,
                sets: Vec::new(),
            })
        }
        Err(e) => return Err(e.into()),
    };
    let mut sets = Vec::new();
    for id in lib.list_sets()? {
        match lib.load_manifest(&id) {
            Ok(m) => {
                let p = &m.provenance;
                let fixture = p.game.is_fixture()
                    || p.platform == Platform::Synthetic
                    || p.sources.iter().any(|s| matches!(s, SourceRef::Fixture));
                let label = if fixture && !p.game.is_fixture() {
                    format!("FIXTURE (marked synthetic): {}", p.game.label())
                } else {
                    p.game.label().to_string()
                };
                sets.push(SetListing {
                    set_id: id,
                    label: Some(label),
                    fixture,
                    summary: Some(m.summary()),
                    files: Some(m.entries.len()),
                    bytes: Some(m.total_bytes()),
                    imported_at: Some(m.imported_at),
                    importer_version: Some(m.importer_version.clone()),
                    identification_verified: Some(m.is_verified()),
                    identification: m.identification.clone(),
                    sources: p
                        .sources
                        .iter()
                        .map(|s| SourceSummary::from_source(s, show_paths))
                        .collect(),
                    error: None,
                });
            }
            Err(e) => sets.push(SetListing {
                set_id: id,
                label: None,
                fixture: false,
                summary: None,
                files: None,
                bytes: None,
                imported_at: None,
                importer_version: None,
                identification_verified: None,
                identification: None,
                sources: Vec::new(),
                error: Some(e.to_string()),
            }),
        }
    }
    Ok(ListResult {
        command: "list",
        state_dir: shown,
        library_exists: true,
        sets,
    })
}
