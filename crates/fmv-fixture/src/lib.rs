//! FIXTURE adapter — a synthetic, hand-made walkable area used only to exercise networking
//! before owned FF7 data is available.
//!
//! **This is NOT FF7 content and NOT evidence of FF7 support.** Every surface that shows it
//! (server READY line, client window title, logs) must label it FIXTURE.

use fmv_adapter_api::{AdapterError, AreaInfo, AreaRuntime, Bounds, GameAdapter, MoveResult};
use fmv_core::{AreaId, EditionProvenance, Fingerprint, GameId, Platform, Pos, SourceRef};

pub const FIXTURE_AREA_KEY: &str = "plaza";

/// L-shaped plaza made of four triangles (two quads), units roughly match FF7 field scale.
const TRIS: [[(f32, f32); 3]; 4] = [
    // main quad: x -400..400, y -300..300
    [(-400.0, -300.0), (400.0, -300.0), (400.0, 300.0)],
    [(-400.0, -300.0), (400.0, 300.0), (-400.0, 300.0)],
    // arm: x 400..700, y -300..0
    [(400.0, -300.0), (700.0, -300.0), (700.0, 0.0)],
    [(400.0, -300.0), (700.0, 0.0), (400.0, 0.0)],
];

pub struct FixtureAdapter {
    info: AreaInfo,
}

impl Default for FixtureAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl FixtureAdapter {
    pub fn new() -> Self {
        let mut h = blake3::Hasher::new();
        h.update(b"fmv-fixture/plaza/v1");
        for t in TRIS {
            for (x, y) in t {
                h.update(&x.to_le_bytes());
                h.update(&y.to_le_bytes());
            }
        }
        let info = AreaInfo {
            id: AreaId { game: GameId::Fixture, key: FIXTURE_AREA_KEY.to_string() },
            display_name: "FIXTURE plaza (synthetic, not FF7)".to_string(),
            fingerprint: Fingerprint(*h.finalize().as_bytes()),
            provenance: EditionProvenance {
                game: GameId::Fixture,
                platform: Platform::Synthetic,
                edition: "fixture".into(),
                region: "none".into(),
                language: "none".into(),
                revision: "v1".into(),
                sources: vec![SourceRef::Fixture],
                modded: false,
            },
            fixture: true,
        };
        Self { info }
    }
}

impl GameAdapter for FixtureAdapter {
    fn game(&self) -> GameId {
        GameId::Fixture
    }
    fn list_areas(&self) -> Vec<AreaInfo> {
        vec![self.info.clone()]
    }
    fn load_area(&self, key: &str) -> Result<Box<dyn AreaRuntime>, AdapterError> {
        if key != FIXTURE_AREA_KEY {
            return Err(AdapterError::AreaNotFound(key.to_string()));
        }
        Ok(Box::new(FixtureArea { info: self.info.clone() }))
    }
}

pub struct FixtureArea {
    info: AreaInfo,
}

fn inside(t: &[(f32, f32); 3], p: (f32, f32)) -> bool {
    let s = |a: (f32, f32), b: (f32, f32), c: (f32, f32)| (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0);
    let d1 = s(t[0], t[1], p);
    let d2 = s(t[1], t[2], p);
    let d3 = s(t[2], t[0], p);
    let neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(neg && pos)
}

fn on_mesh(p: (f32, f32)) -> bool {
    TRIS.iter().any(|t| inside(t, p))
}

impl AreaRuntime for FixtureArea {
    fn info(&self) -> &AreaInfo {
        &self.info
    }
    fn spawn_points(&self) -> Vec<Pos> {
        vec![Pos::new(-100.0, 0.0, 0.0), Pos::new(100.0, 0.0, 0.0), Pos::new(0.0, 100.0, 0.0), Pos::new(0.0, -100.0, 0.0)]
    }
    fn validate_move(&self, from: Pos, to: Pos) -> MoveResult {
        if to.is_finite() && on_mesh((to.x, to.y)) {
            MoveResult::Accepted(Pos::new(to.x, to.y, 0.0))
        } else if from.is_finite() && on_mesh((from.x, from.y)) {
            MoveResult::Corrected(Pos::new(from.x, from.y, 0.0))
        } else {
            MoveResult::Corrected(self.spawn_points()[0])
        }
    }
    fn bounds(&self) -> Bounds {
        Bounds { min: Pos::new(-400.0, -300.0, 0.0), max: Pos::new(700.0, 300.0, 0.0) }
    }
    fn walk_triangles(&self) -> Vec<[Pos; 3]> {
        TRIS.iter().map(|t| t.map(|(x, y)| Pos::new(x, y, 0.0))).collect()
    }
    fn max_speed(&self) -> f32 {
        // FIXTURE value; real FF7 walk/run speeds come from the FF7 adapter.
        600.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_moves() {
        let a = FixtureAdapter::new();
        assert!(a.info.fixture);
        let area = a.load_area(FIXTURE_AREA_KEY).unwrap();
        let from = Pos::new(0.0, 0.0, 0.0);
        assert!(matches!(area.validate_move(from, Pos::new(600.0, -100.0, 0.0)), MoveResult::Accepted(_)));
        assert_eq!(area.validate_move(from, Pos::new(600.0, 200.0, 0.0)), MoveResult::Corrected(from));
        assert!(matches!(area.validate_move(from, Pos::new(f32::NAN, 0.0, 0.0)), MoveResult::Corrected(_)));
        assert!(a.load_area("nope").is_err());
    }
}
