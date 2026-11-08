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
}

/// A game adapter bound to one owned-content source (or the synthetic fixture).
pub trait GameAdapter: Send + Sync {
    fn game(&self) -> GameId;
    fn list_areas(&self) -> Vec<AreaInfo>;
    fn load_area(&self, key: &str) -> Result<Box<dyn AreaRuntime>, AdapterError>;
}
