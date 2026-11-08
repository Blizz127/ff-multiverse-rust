//! Adapter trait boundary. Server and client link adapters only through these traits,
//! so new games (FF8/9/10/12) never require core changes.

use fmv_core::{AreaId, EditionProvenance, Fingerprint, GameId, Pos};

#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error("area not found: {0}")]
    AreaNotFound(String),
    #[error("owned content missing: {0}")]
    MissingContent(String),
    #[error("unsupported format: {0}")]
    Unsupported(String),
    #[error("decode error: {0}")]
    Decode(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Description of one loadable area. `fixture == true` means synthetic content that must be
/// labeled as such everywhere and is not evidence of real game support.
#[derive(Debug, Clone)]
pub struct AreaInfo {
    pub id: AreaId,
    pub display_name: String,
    pub fingerprint: Fingerprint,
    pub provenance: EditionProvenance,
    pub fixture: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MoveResult {
    /// Destination is valid (possibly with height snapped to the walkable surface).
    Accepted(Pos),
    /// Destination invalid; entity must be placed at this corrected position instead.
    Corrected(Pos),
}

impl MoveResult {
    pub fn pos(self) -> Pos {
        match self {
            MoveResult::Accepted(p) | MoveResult::Corrected(p) => p,
        }
    }
}

/// Axis-aligned bounds of the walkable area in adapter space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bounds {
    pub min: Pos,
    pub max: Pos,
}

/// One possible random encounter of an area: which battle, and how likely relative to the
/// other entries of the same table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncounterEntry {
    /// The adapter's own battle id (FF7: the formation id, `scene * 4 + slot`).
    pub formation: u16,
    /// Relative likelihood within the table (FF7 stores it out of 64). `0` is never picked.
    pub weight: u16,
}

/// An area's random-encounter table: how often walking triggers a roll, and what it can turn up.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EncounterTable {
    /// How easily a step triggers an encounter, on the adapter's own `0..=255` scale. `0` means
    /// the area has no random encounters.
    pub rate: u8,
    pub entries: Vec<EncounterEntry>,
}

impl EncounterTable {
    /// Sum of the weights (the exclusive upper bound of [`EncounterTable::pick`]'s roll).
    pub fn total_weight(&self) -> u32 {
        self.entries.iter().map(|e| u32::from(e.weight)).sum()
    }

    /// True when walking can never trigger a battle here (rate 0, or nothing to pick).
    pub fn is_empty(&self) -> bool {
        self.rate == 0 || self.total_weight() == 0
    }

    /// The formation a `roll` in `0..total_weight()` selects, the weights laid end to end in
    /// entry order. `None` if the table is empty or `roll` is out of range.
    pub fn pick(&self, roll: u32) -> Option<u16> {
        let mut rest = roll;
        for e in &self.entries {
            let w = u32::from(e.weight);
            if rest < w {
                return Some(e.formation);
            }
            rest -= w;
        }
        None
    }
}

/// Loaded, server- or client-side runtime for one area. Holds decoded data from the local
/// owned copy only; nothing here is ever serialized onto the network.
pub trait AreaRuntime: Send + Sync {
    fn info(&self) -> &AreaInfo;
    fn spawn_points(&self) -> Vec<Pos>;
    /// Validate a move from `from` to `to` against the area's walkable surface.
    fn validate_move(&self, from: Pos, to: Pos) -> MoveResult;
    fn bounds(&self) -> Bounds;
    /// Walkable triangles for debug rendering / overlays.
    fn walk_triangles(&self) -> Vec<[Pos; 3]>;
    /// Maximum allowed movement speed in adapter units per second (server enforces).
    fn max_speed(&self) -> f32;
    /// The area's random-encounter table, if it has one (the server rolls it as players walk).
    /// Additive in protocol v3: adapters that do not know encounters keep the default `None`.
    fn encounters(&self) -> Option<EncounterTable> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(rate: u8, weights: &[(u16, u16)]) -> EncounterTable {
        EncounterTable {
            rate,
            entries: weights.iter().map(|&(formation, weight)| EncounterEntry { formation, weight }).collect(),
        }
    }

    #[test]
    fn picking_lays_the_weights_end_to_end() {
        let t = table(8, &[(10, 3), (20, 0), (30, 61)]);
        assert_eq!(t.total_weight(), 64);
        let picked: Vec<u16> = (0..64).map(|r| t.pick(r).unwrap()).collect();
        assert_eq!(picked.iter().filter(|&&f| f == 10).count(), 3);
        assert_eq!(picked.iter().filter(|&&f| f == 30).count(), 61);
        assert!(!picked.contains(&20), "a weight of zero is never picked");
        assert_eq!(&picked[..4], &[10, 10, 10, 30]);
        assert_eq!(t.pick(64), None, "out of range");
    }

    #[test]
    fn a_table_without_rate_or_weight_is_empty() {
        assert!(table(0, &[(1, 64)]).is_empty());
        assert!(table(9, &[]).is_empty());
        assert!(table(9, &[(1, 0)]).is_empty());
        assert!(!table(1, &[(1, 1)]).is_empty());
        assert!(EncounterTable::default().is_empty());
        assert_eq!(EncounterTable::default().pick(0), None);
    }
}

/// A game adapter bound to one owned-content source (or the synthetic fixture).
pub trait GameAdapter: Send + Sync {
    fn game(&self) -> GameId;
    fn list_areas(&self) -> Vec<AreaInfo>;
    fn load_area(&self, key: &str) -> Result<Box<dyn AreaRuntime>, AdapterError>;
}
