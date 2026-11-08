//! Where everyone stands in a battle and which way they face.
//!
//! The enemies stand where the formation says (battle units, see [`super::camera`] for the
//! frame); without placement data they stand in rows of three. The party is not in the game's
//! files (the original engine places it itself): it is a line on the side opposite the enemies,
//! its first member farthest from the usual camera, each a little further out than the one
//! before, so that the line is a diagonal like the game's. A battle whose enemies stand behind
//! the party (a back attack) puts the party on the other side; a pincer (enemies on both sides)
//! puts it in the middle. Everyone faces the other side's centre.
//!
//! The numbers are made up to look right, not read from the game: [`PARTY_DEPTH`] and the
//! others are named so that they can be tuned in one place.

use fmv_protocol::battle::FormationScene;

/// How far from the middle of the field the party's line stands (towards the side opposite the enemies).
pub const PARTY_DEPTH: f32 = 2000.0;
/// Distance between neighbours of the party's line, across the field.
pub const PARTY_SPACING: f32 = 1100.0;
/// How much further out each party member stands than the one before. From the side the usual
/// camera looks from, the line runs towards the camera, and this is what keeps its members from
/// standing in each other's way: on the screen they form a diagonal.
pub const PARTY_STAGGER: f32 = 520.0;
/// Enemies of a formation without placement data: distance between neighbours of a row, how far
/// the first row is from the middle and the gap between rows.
pub const ENEMY_SPACING: f32 = 1700.0;
pub const ENEMY_DEPTH: f32 = 2400.0;
pub const ENEMY_ROW_GAP: f32 = 1800.0;
/// An enemy this far from the middle line (`z = 0`) or more counts as standing on one side of it.
const SIDE_BAND: f32 = 300.0;

/// Where one combatant stands and the way it faces.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spot {
    /// `x`, `y` (down; the ground is 0, a flying enemy is negative) and `z`.
    pub pos: [f32; 3],
    /// The heading: `forward = (sin yaw, cos yaw)` on the ground's `(x, z)`, so yaw 0 faces `+z` and
    /// `PI` faces `-z`.
    pub yaw: f32,
}

impl Spot {
    /// The unit direction on the ground `(x, z)` the combatant faces (tests).
    #[cfg(test)]
    pub fn forward(&self) -> [f32; 2] {
        [self.yaw.sin(), self.yaw.cos()]
    }

    /// The ground position `(x, z)`.
    pub fn ground(&self) -> [f32; 2] {
        [self.pos[0], self.pos[2]]
    }
}

/// Everyone's place.
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    /// One per party member, in the engine's order.
    pub party: Vec<Spot>,
    /// One per enemy, in the engine's order.
    pub enemies: Vec<Spot>,
}

/// The heading that faces `to` from `from` (ground positions `(x, z)`), `None` when they coincide.
pub fn yaw_towards(from: [f32; 2], to: [f32; 2]) -> Option<f32> {
    let (dx, dz) = (to[0] - from[0], to[1] - from[1]);
    (dx.hypot(dz) >= 1.0 && dx.is_finite() && dz.is_finite()).then(|| dx.atan2(dz))
}

/// Where enemy `k` of `n` stands in a formation without placement data: rows of three, centred.
fn default_enemy(k: usize, n: usize) -> [f32; 3] {
    let (row, col) = (k / 3, k % 3);
    let in_row = (n - row * 3).min(3);
    let x = (col as f32 - (in_row as f32 - 1.0) * 0.5) * ENEMY_SPACING;
    [x, 0.0, -(ENEMY_DEPTH + row as f32 * ENEMY_ROW_GAP)]
}

/// The side the party stands on: `1.0` for positive `z`, `-1.0` for negative `z`, `0.0` for the middle.
fn party_side(enemies: &[[f32; 3]]) -> f32 {
    let (behind, ahead) = enemies.iter().fold((0, 0), |(neg, pos), e| match e[2] {
        z if z < -SIDE_BAND => (neg + 1, pos),
        z if z > SIDE_BAND => (neg, pos + 1),
        _ => (neg, pos),
    });
    match (behind, ahead) {
        (n, p) if n > 0 && p > 0 => 0.0,
        (n, p) if p > n => -1.0,
        _ => 1.0,
    }
}

/// The centre of some ground positions (`None` for none).
fn centre(points: impl Iterator<Item = [f32; 2]>) -> Option<[f32; 2]> {
    let (sum, n) = points.fold(([0.0f32; 2], 0usize), |(s, n), p| ([s[0] + p[0], s[1] + p[1]], n + 1));
    (n > 0).then(|| [sum[0] / n as f32, sum[1] / n as f32])
}

/// Place `enemy_count` enemies and `party_count` party members. `formation` supplies the enemies'
/// own places (entry `k` for enemy `k`; an enemy without an entry gets a default one).
pub fn arrange(formation: Option<&FormationScene>, enemy_count: usize, party_count: usize) -> Layout {
    let enemy_positions: Vec<[f32; 3]> = (0..enemy_count)
        .map(|k| {
            formation
                .and_then(|f| f.enemies.get(k))
                .map(|e| e.position.map(f32::from))
                .unwrap_or_else(|| default_enemy(k, enemy_count))
        })
        .collect();
    let side = party_side(&enemy_positions);
    let middle = (party_count as f32 - 1.0) * 0.5;
    let party_positions: Vec<[f32; 3]> = (0..party_count)
        .map(|i| {
            let along = i as f32 - middle;
            if side == 0.0 {
                [along * PARTY_SPACING * 0.9, 0.0, along * PARTY_STAGGER]
            } else {
                [along * PARTY_SPACING, 0.0, side * (PARTY_DEPTH + along * PARTY_STAGGER)]
            }
        })
        .collect();

    let enemy_centre = centre(enemy_positions.iter().map(|p| [p[0], p[2]]));
    let party_centre = centre(party_positions.iter().map(|p| [p[0], p[2]]));
    // Facing the other side's centre; two sides on top of each other face the way they would face
    // from their usual sides.
    let party_default = if side < 0.0 { 0.0 } else { std::f32::consts::PI };
    let face = |from: [f32; 3], target: Option<[f32; 2]>, default: f32| {
        target.and_then(|t| yaw_towards([from[0], from[2]], t)).unwrap_or(default)
    };
    Layout {
        party: party_positions.iter().map(|&pos| Spot { pos, yaw: face(pos, enemy_centre, party_default) }).collect(),
        enemies: enemy_positions
            .iter()
            .map(|&pos| Spot { pos, yaw: face(pos, party_centre, party_default + std::f32::consts::PI) })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmv_ff7_battle::scene::BattleLayout;
    use fmv_protocol::battle::{CameraPose, EnemyPlacement};

    fn formation(positions: &[[i16; 3]]) -> FormationScene {
        FormationScene {
            location: 0,
            layout: BattleLayout::Normal,
            camera: CameraPose { position: [9000, -2400, 1800], target: [0, -300, 100] },
            enemies: positions.iter().enumerate().map(|(i, &position)| EnemyPlacement { enemy_id: i as u16, position, row: 1 }).collect(),
        }
    }

    fn facing(spot: &Spot, target: [f32; 2]) -> f32 {
        let (dx, dz) = (target[0] - spot.pos[0], target[1] - spot.pos[2]);
        let len = dx.hypot(dz);
        let f = spot.forward();
        (f[0] * dx + f[1] * dz) / len
    }

    #[test]
    fn enemies_stand_where_the_formation_says_and_the_party_on_the_opposite_side_facing_them() {
        let f = formation(&[[-600, 0, -1400], [550, 0, -1800]]);
        let layout = arrange(Some(&f), 2, 3);
        assert_eq!(layout.enemies.len(), 2);
        assert_eq!(layout.enemies[0].pos, [-600.0, 0.0, -1400.0]);
        assert_eq!(layout.enemies[1].pos, [550.0, 0.0, -1800.0]);
        assert_eq!(layout.party.len(), 3);
        for p in &layout.party {
            assert!(p.pos[2] > 1000.0, "the party stands on the positive-z side: {p:?}");
            assert_eq!(p.pos[1], 0.0, "on the ground");
        }
        // Every party member looks at the enemies' centre, every enemy at the party's.
        let enemy_centre = [-25.0, -1600.0];
        for p in &layout.party {
            assert!(facing(p, enemy_centre) > 0.999, "{p:?}");
        }
        let party_centre = centre(layout.party.iter().map(Spot::ground)).unwrap();
        for e in &layout.enemies {
            assert!(facing(e, party_centre) > 0.999, "{e:?}");
        }
        // The line is spread across the field and staggered: neighbours differ in x and in z.
        assert!(layout.party[1].pos[0] - layout.party[0].pos[0] > 500.0);
        assert!((layout.party[1].pos[2] - layout.party[0].pos[2]).abs() > 100.0);
    }

    #[test]
    fn a_flying_enemy_keeps_its_height_and_a_lone_hero_stands_in_the_middle() {
        let layout = arrange(Some(&formation(&[[0, -700, -3000]])), 1, 1);
        assert_eq!(layout.enemies[0].pos[1], -700.0);
        assert_eq!(layout.party[0].pos[0], 0.0, "a party of one is in the middle of the line");
        assert!((layout.party[0].pos[2] - PARTY_DEPTH).abs() < 1e-3);
    }

    #[test]
    fn a_back_attack_puts_the_party_on_the_other_side_and_a_pincer_in_the_middle() {
        let behind = formation(&[[-2500, 0, 1700], [2500, 0, 1700]]);
        let layout = arrange(Some(&behind), 2, 3);
        for p in &layout.party {
            assert!(p.pos[2] < -1000.0, "enemies at positive z: the party is at negative z, {p:?}");
            assert!(facing(p, [0.0, 1700.0]) > 0.999);
        }
        let pincer = formation(&[[-1000, 0, -3000], [1000, 0, -3000], [-1000, 0, 3000], [1000, 0, 3000]]);
        let layout = arrange(Some(&pincer), 4, 3);
        for p in &layout.party {
            assert!(p.pos[2].abs() <= PARTY_STAGGER + 1.0, "enemies on both sides: the party is in the middle, {p:?}");
        }
        // The party's centre is the middle of the field, so facing is whatever the centre says: finite.
        assert!(layout.party.iter().chain(&layout.enemies).all(|s| s.yaw.is_finite()));
    }

    #[test]
    fn without_placement_data_enemies_stand_in_centred_rows_of_three() {
        let layout = arrange(None, 5, 2);
        let xs: Vec<f32> = layout.enemies.iter().map(|e| e.pos[0]).collect();
        // A row of three, then a row of two centred on the same middle.
        assert_eq!(xs[..3], [-ENEMY_SPACING, 0.0, ENEMY_SPACING]);
        assert!((xs[3] + ENEMY_SPACING * 0.5).abs() < 1e-3 && (xs[4] - ENEMY_SPACING * 0.5).abs() < 1e-3, "{xs:?}");
        assert!(layout.enemies[3].pos[2] < layout.enemies[0].pos[2], "the second row is further back");
        // An enemy the data has no entry for gets a default place; the others keep theirs.
        let partial = formation(&[[100, 0, -900]]);
        let layout = arrange(Some(&partial), 2, 1);
        assert_eq!(layout.enemies[0].pos, [100.0, 0.0, -900.0]);
        assert_eq!(layout.enemies[1].pos, default_enemy(1, 2));
    }

    #[test]
    fn nobody_at_all_and_overfull_sides_are_fine() {
        let empty = arrange(None, 0, 0);
        assert!(empty.party.is_empty() && empty.enemies.is_empty());
        let full = arrange(None, 32, 8);
        assert_eq!((full.party.len(), full.enemies.len()), (8, 32));
        for s in full.party.iter().chain(&full.enemies) {
            assert!(s.pos.iter().all(|c| c.is_finite()) && s.yaw.is_finite(), "{s:?}");
        }
        // Everybody stands on a distinct spot.
        for (i, a) in full.enemies.iter().enumerate() {
            for b in &full.enemies[i + 1..] {
                assert!(a.pos != b.pos || i >= 6 * 5, "two enemies on one spot");
            }
        }
        // A party and enemies on the same spot do not make a NaN heading.
        let on_top = arrange(Some(&formation(&[[0, 0, 0]])), 1, 1);
        assert!(on_top.party[0].yaw.is_finite() && on_top.enemies[0].yaw.is_finite());
    }

    #[test]
    fn the_heading_convention_is_sin_cos_of_x_z() {
        assert_eq!(yaw_towards([0.0, 0.0], [0.0, 10.0]), Some(0.0), "+z is yaw 0");
        assert!((yaw_towards([0.0, 0.0], [10.0, 0.0]).unwrap() - std::f32::consts::FRAC_PI_2).abs() < 1e-6, "+x is a quarter turn");
        assert!((yaw_towards([0.0, 0.0], [0.0, -10.0]).unwrap().abs() - std::f32::consts::PI).abs() < 1e-6, "-z is a half turn");
        assert_eq!(yaw_towards([1.0, 1.0], [1.2, 1.2]), None, "too close to have a direction");
        let s = Spot { pos: [0.0; 3], yaw: std::f32::consts::FRAC_PI_2 };
        assert!((s.forward()[0] - 1.0).abs() < 1e-6 && s.forward()[1].abs() < 1e-6);
    }
}
