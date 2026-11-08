//! Opening the FF7 adapter over the owner's own content.
//!
//! Finding the archive (library set, layout, direct path) is `fmv_library::ff7`: the server and
//! every client resolve with the same code, because the area fingerprint binds the provenance
//! that resolution returns. This module only turns the result into an [`Ff7Adapter`], indexes
//! it, and names the adapter for build strings.
//!
//! It also opens the server's own battle data (`scene.bin` and `kernel.bin` of the same library
//! set, [`open_battle_data`]) and reads each field's random-encounter table out of the field file
//! ([`decode_encounter_section`]), which [`SharedFf7`] attaches to the areas it loads (the FF7
//! adapter itself does not decode encounters).
//!
//! Everything here opens files read-only. Local paths appear in errors shown on this machine
//! only; the provenance that goes on the wire is `EditionProvenance::summary()`, which has none.

use std::sync::Arc;
use std::time::{Duration, Instant};

use fmv_adapter_api::{
    AdapterError, AreaInfo, AreaRuntime, Bounds, EncounterEntry, EncounterTable, GameAdapter, MoveResult,
};
use fmv_core::{GameId, Pos};
use fmv_ff7::field::section;
use fmv_ff7::{Ff7Adapter, Ff7FieldArea};
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
/// and then handed to the server as a `Box<dyn GameAdapter>`. The areas it loads carry the field's
/// random-encounter table (see [`decode_encounter_section`]).
pub struct SharedFf7(pub Arc<Ff7Adapter>);

impl GameAdapter for SharedFf7 {
    fn game(&self) -> GameId {
        self.0.game()
    }
    fn list_areas(&self) -> Vec<AreaInfo> {
        self.0.list_areas()
    }
    fn load_area(&self, key: &str) -> Result<Box<dyn AreaRuntime>, AdapterError> {
        let field = self.0.load_field(key)?;
        let encounters = field.field_file().section(section::ENCOUNTER).ok().and_then(decode_encounter_section);
        Ok(Box::new(WithEncounters { inner: field, encounters }))
    }
}

/// An FF7 field plus the random-encounter table decoded from its own file. Everything else is
/// the field's.
struct WithEncounters {
    inner: Ff7FieldArea,
    encounters: Option<EncounterTable>,
}

impl AreaRuntime for WithEncounters {
    fn info(&self) -> &AreaInfo {
        self.inner.info()
    }
    fn spawn_points(&self) -> Vec<Pos> {
        self.inner.spawn_points()
    }
    fn validate_move(&self, from: Pos, to: Pos) -> MoveResult {
        self.inner.validate_move(from, to)
    }
    fn bounds(&self) -> Bounds {
        self.inner.bounds()
    }
    fn walk_triangles(&self) -> Vec<[Pos; 3]> {
        self.inner.walk_triangles()
    }
    fn max_speed(&self) -> f32 {
        self.inner.max_speed()
    }
    fn encounters(&self) -> Option<EncounterTable> {
        self.encounters.clone()
    }
}

/// Bytes of one encounter table in a PC field's encounter section (the section holds two).
const ENCOUNTER_TABLE_LEN: usize = 24;
/// Standard encounters per table.
const ENCOUNTER_SLOTS: usize = 6;

/// Decode the random-encounter table of an FF7 PC field from its encounter section (index 6).
///
/// Layout (community documentation, CHECKED against the owner's Steam copy: see the real-data
/// test `every_field_encounter_table_decodes_and_names_real_formations`): two tables of 24 bytes.
/// Each is `u8 enabled`, `u8 rate`, six `u16` little-endian "standard" entries (the low 10 bits
/// are the formation id, `scene * 4 + slot`, the high 6 bits the weight; the weights of a used
/// table add up to 64) and then four "special" entries (back attacks and the like) and padding
/// that are not decoded (APPROXIMATION: a battle is never a special one). The first table that is
/// enabled and has an entry is the area's; `None` when neither is (no random battles there).
pub fn decode_encounter_section(bytes: &[u8]) -> Option<EncounterTable> {
    bytes.as_chunks::<ENCOUNTER_TABLE_LEN>().0.iter().find_map(|table| {
        let (enabled, rate) = (*table.first()?, *table.get(1)?);
        if enabled == 0 || rate == 0 {
            return None;
        }
        let entries: Vec<EncounterEntry> = table
            .get(2..2 + 2 * ENCOUNTER_SLOTS)?
            .as_chunks::<2>()
            .0
            .iter()
            .filter_map(|pair| {
                let raw = u16::from_le_bytes(*pair);
                let (formation, weight) = (raw & 0x03FF, raw >> 10);
                (weight > 0).then_some(EncounterEntry { formation, weight })
            })
            .collect();
        let table = EncounterTable { rate, entries };
        (!table.is_empty()).then_some(table)
    })
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

    /// A 24-byte table: enabled flag, rate, up to six `(formation, weight)` standard entries.
    fn table_bytes(enabled: u8, rate: u8, entries: &[(u16, u16)]) -> Vec<u8> {
        let mut t = vec![enabled, rate];
        for slot in 0..ENCOUNTER_SLOTS {
            let raw = entries.get(slot).map_or(0, |&(formation, weight)| {
                assert!(formation < 1024 && weight < 64, "a formation is 10 bits and a weight 6 bits");
                (weight << 10) | formation
            });
            t.extend_from_slice(&raw.to_le_bytes());
        }
        t.resize(ENCOUNTER_TABLE_LEN, 0);
        t
    }

    #[test]
    fn an_enabled_table_decodes_to_formations_and_weights() {
        let mut section = table_bytes(1, 128, &[(301, 24), (302, 22), (303, 18)]);
        section.extend(table_bytes(0, 0, &[]));
        let t = decode_encounter_section(&section).expect("a table");
        assert_eq!(t.rate, 128);
        assert_eq!(
            t.entries,
            vec![
                EncounterEntry { formation: 301, weight: 24 },
                EncounterEntry { formation: 302, weight: 22 },
                EncounterEntry { formation: 303, weight: 18 },
            ]
        );
        assert_eq!(t.total_weight(), 64, "the weights of a real table add up to 64");
        // The low 10 bits are the formation: the largest id fits, the weight is only the high 6 bits.
        let big = decode_encounter_section(&table_bytes(1, 5, &[(1023, 63)])).unwrap();
        assert_eq!(big.entries, vec![EncounterEntry { formation: 1023, weight: 63 }]);
    }

    #[test]
    fn the_first_usable_table_wins_and_disabled_or_empty_ones_mean_no_battles() {
        // Table 1 disabled, table 2 enabled: table 2.
        let mut s = table_bytes(0, 200, &[(1, 63)]);
        s.extend(table_bytes(1, 10, &[(7, 63)]));
        assert_eq!(decode_encounter_section(&s).unwrap().entries, vec![EncounterEntry { formation: 7, weight: 63 }]);
        // Both enabled: table 1.
        let mut s = table_bytes(1, 10, &[(1, 63)]);
        s.extend(table_bytes(1, 10, &[(7, 63)]));
        assert_eq!(decode_encounter_section(&s).unwrap().entries[0].formation, 1);
        // Enabled but rate 0, or enabled with nothing in it, or all zero: no battles.
        assert_eq!(decode_encounter_section(&table_bytes(1, 0, &[(1, 63)])), None);
        assert_eq!(decode_encounter_section(&table_bytes(1, 9, &[])), None);
        assert_eq!(decode_encounter_section(&[0u8; 48]), None);
        // Unused slots (zero) are not entries.
        let sparse = decode_encounter_section(&table_bytes(1, 9, &[(0, 0), (5, 63)])).unwrap();
        assert_eq!(sparse.entries, vec![EncounterEntry { formation: 5, weight: 63 }]);
    }

    #[test]
    fn a_section_of_the_wrong_shape_decodes_to_nothing_instead_of_panicking() {
        for bytes in [&[][..], &[1u8][..], &[1, 2, 3][..], &[1u8; 23][..]] {
            assert_eq!(decode_encounter_section(bytes), None, "{} bytes", bytes.len());
        }
        // Trailing bytes after the tables are ignored; a short tail is not a table.
        let mut s = table_bytes(1, 9, &[(5, 63)]);
        s.extend([9u8; 7]);
        assert!(decode_encounter_section(&s).is_some());
        // A hostile section of all-ones is a table of formation 1023, weight 63: harmless data.
        assert!(decode_encounter_section(&[0xffu8; 48]).is_some());
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
