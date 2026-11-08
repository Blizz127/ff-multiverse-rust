//! Opening the FF7 adapter over the owner's own content.
//!
//! Finding the archive (library set, layout, direct path) is `fmv_library::ff7`: the server and
//! every client resolve with the same code, because the area fingerprint binds the provenance
//! that resolution returns. This module only turns the result into an [`Ff7Adapter`], indexes
//! it, and names the adapter for build strings.
//!
//! It also opens the server's own battle data (`scene.bin` and `kernel.bin` of the same library
//! set, [`open_battle_data`]). A field's random-encounter table is decoded by the adapter itself
//! (`fmv_ff7::decode_encounter_section`, re-exported here for the callers that read tables
//! straight out of a field file); the areas [`SharedFf7`] loads are the adapter's own.
//!
//! Everything here opens files read-only. Local paths appear in errors shown on this machine
//! only; the provenance that goes on the wire is `EditionProvenance::summary()`, which has none.

use std::sync::Arc;
use std::time::{Duration, Instant};

use fmv_adapter_api::{AdapterError, AreaInfo, AreaRuntime, GameAdapter};
use fmv_core::GameId;
pub use fmv_ff7::decode_encounter_section;
use fmv_ff7::Ff7Adapter;
pub use fmv_library::ff7::{
    resolve_battle_files, resolve_flevel, Ff7Labels, Ff7ResolveError, Ff7Source, ResolvedBattleFiles, ResolvedFlevel,
};
use fmv_protocol::{Ff7BattleData, Ff7BattleDataError};

/// The FF7 field hosted when `--area` is not given (the Sector 1 station platform).
pub const DEFAULT_FF7_AREA: &str = "md1stin";

#[derive(Debug, thiserror::Error)]
pub enum Ff7OpenError {
    #[error(transparent)]
    Resolve(#[from] Ff7ResolveError),
    #[error(transparent)]
    Adapter(#[from] AdapterError),
}

/// An opened adapter, shareable so it can be prewarmed on a blocking thread and then handed to
/// the server (see [`SharedFf7`]).
#[derive(Clone)]
pub struct OpenedFf7 {
    pub adapter: Arc<Ff7Adapter>,
    pub origin: String,
}

impl std::fmt::Debug for OpenedFf7 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenedFf7").field("origin", &self.origin).finish_non_exhaustive()
    }
}

/// Resolve and open the adapter (reads only the archive's table of contents; call
/// [`prewarm`] to build the area index).
pub fn open_ff7(source: &Ff7Source) -> Result<OpenedFf7, Ff7OpenError> {
    let resolved = resolve_flevel(source)?;
    let adapter = Ff7Adapter::from_flevel_path(&resolved.path, resolved.provenance)?;
    Ok(OpenedFf7 { adapter: Arc::new(adapter), origin: resolved.origin })
}

/// What [`prewarm`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrewarmReport {
    /// Areas the archive offers.
    pub areas: usize,
    /// Archive entries that are not offered (the name list, unusable names, unreadable entries).
    pub skipped: usize,
    pub elapsed: Duration,
}

/// Build the adapter's area index on a blocking thread (it reads and hashes the whole archive),
/// so that neither the first handshake nor the async executor pays for it.
pub async fn prewarm(adapter: &Arc<Ff7Adapter>) -> Result<PrewarmReport, tokio::task::JoinError> {
    let adapter = Arc::clone(adapter);
    tokio::task::spawn_blocking(move || {
        let started = Instant::now();
        let areas = adapter.prewarm();
        PrewarmReport { areas, skipped: adapter.skipped_entries().len(), elapsed: started.elapsed() }
    })
    .await
}

/// `GameAdapter` over a shared [`Ff7Adapter`]: the same adapter is prewarmed through its `Arc`
/// and then handed to the server as a `Box<dyn GameAdapter>`. The areas it loads are the
/// adapter's own, which carry the field's exits, NPCs and random-encounter table.
pub struct SharedFf7(pub Arc<Ff7Adapter>);

impl GameAdapter for SharedFf7 {
    fn game(&self) -> GameId {
        self.0.game()
    }
    fn list_areas(&self) -> Vec<AreaInfo> {
        self.0.list_areas()
    }
    fn load_area(&self, key: &str) -> Result<Box<dyn AreaRuntime>, AdapterError> {
        self.0.load_area(key)
    }
}

/// Why the battle data cannot be opened.
#[derive(Debug, thiserror::Error)]
pub enum BattleOpenError {
    #[error(transparent)]
    Resolve(#[from] Ff7ResolveError),
    #[error(transparent)]
    Data(#[from] Ff7BattleDataError),
}

/// Open this machine's own `scene.bin` and `kernel.bin` (read-only) from the same library set as
/// the field archive, with a path-free description of where they came from. A direct
/// `--ff7-flevel` has none.
pub fn open_battle_data(source: &Ff7Source) -> Result<(Arc<Ff7BattleData>, String), BattleOpenError> {
    let files = resolve_battle_files(source)?;
    let data = Ff7BattleData::open(&files.scene, &files.kernel)?;
    Ok((Arc::new(data), files.origin))
}

/// The adapter's identity for build strings: crate version and fingerprint rule version
/// (`fmv_ff7::adapter_version`), with the whitespace replaced so it is one token on the READY
/// line. Two builds with different fingerprint rules compute different fingerprints for
/// identical media, so a mismatch must be attributable to the builds and not to the players.
pub fn build_tag() -> String {
    fmv_ff7::adapter_version().split_whitespace().collect::<Vec<_>>().join("+")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_archive_is_an_adapter_error_without_a_path() {
        let err = open_ff7(&Ff7Source::Flevel { path: "/definitely/not/here/flevel.lgp".into(), labels: Ff7Labels::default() })
            .unwrap_err();
        assert!(matches!(err, Ff7OpenError::Adapter(AdapterError::MissingContent(_))), "{err}");
        assert!(!err.to_string().contains("definitely"), "{err}");
    }

    #[test]
    fn library_problems_surface_as_resolve_errors_that_point_at_fmv_import() {
        let empty = std::env::temp_dir().join("fmv-server-test-no-such-library-dir");
        let err = open_ff7(&Ff7Source::Library { state_dir: Some(empty), set: None }).unwrap_err();
        assert!(matches!(err, Ff7OpenError::Resolve(Ff7ResolveError::NoLibrary { .. })), "{err}");
        assert!(err.to_string().contains("needs owned FF7 content imported via fmv-import"), "{err}");
    }

    #[test]
    fn the_build_tag_is_one_token() {
        let t = build_tag();
        assert!(t.starts_with("fmv-ff7/"), "{t}");
        assert!(!t.contains(char::is_whitespace), "{t}");
        assert!(t.contains("fingerprint-rule/"), "{t}");
    }

    #[test]
    fn the_default_area_is_the_station_platform() {
        assert_eq!(DEFAULT_FF7_AREA, "md1stin");
    }

    // The encounter-table decoder moved to `fmv_ff7::decode_encounter_section` with its unit tests
    // (`fmv-ff7/src/encounter.rs`); the server only re-exports it.
    #[test]
    fn the_encounter_decoder_is_still_reachable_from_here() {
        let mut section = vec![1, 128];
        for raw in [(24u16 << 10) | 301, (22 << 10) | 302, (18 << 10) | 303, 0, 0, 0] {
            section.extend_from_slice(&raw.to_le_bytes());
        }
        section.resize(48, 0);
        let table = decode_encounter_section(&section).expect("a table");
        assert_eq!((table.rate, table.total_weight()), (128, 64));
    }

    #[test]
    fn missing_battle_data_is_a_resolve_error_that_points_at_fmv_import() {
        let empty = std::env::temp_dir().join("fmv-server-test-no-battle-library-dir");
        let err = open_battle_data(&Ff7Source::Library { state_dir: Some(empty), set: None }).unwrap_err();
        assert!(matches!(err, BattleOpenError::Resolve(Ff7ResolveError::NoLibrary { .. })), "{err}");
        assert!(err.to_string().contains("needs owned FF7 content imported via fmv-import"), "{err}");
        let err = open_battle_data(&Ff7Source::Flevel { path: "/x/flevel.lgp".into(), labels: Ff7Labels::default() }).unwrap_err();
        assert!(matches!(err, BattleOpenError::Resolve(Ff7ResolveError::BattleNeedsLibrary)), "{err}");
    }
}
