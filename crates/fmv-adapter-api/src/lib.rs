//! Adapter trait boundary. Server and client link adapters only through these traits,
//! so new games (FF8/9/10/12) never require core changes.

use fmv_core::{AreaId, EditionProvenance, Fingerprint, GameId, PhaseKey, Pos};

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

/// An area's key within its game: what [`GameAdapter::load_area`] takes and `AreaId::key` holds
/// (FF7: the field name, `md1stin`). A plain alias, so keys stay interchangeable with
/// `AreaId::key`; it only says what a string is for.
pub type AreaKey = String;

/// How an [`Exit`] is triggered.
///
/// Only [`ExitKind::Gateway`] exists. The other ways out of an FF7 field are decided by its
/// scripts (a `MAPJUMP` run from a line or a talk script, which needs the script VM and the game's
/// story variables); they are not exits in this sense, and adding them later means adding a kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExitKind {
    /// A gateway of the area's own data (FF7: one of the twelve gateways of the field's trigger
    /// section): walking across its line leads straight to another area, no script involved.
    Gateway,
}

/// One way out of an area: walking across [`Exit::line`] leaves the area for [`Exit::target`] and
/// puts the entity at [`Exit::arrival`] there.
#[derive(Debug, Clone, PartialEq)]
pub struct Exit {
    /// End points of the exit line in THIS area's coordinates. Only x and y take part in the
    /// crossing test; z is the line's own height (FF7 stores one per end point) and is ignored.
    pub line: (Pos, Pos),
    /// The area the exit leads to. It always names an area the adapter lists (an exit to a name the
    /// adapter does not have is not reported), but that area may still fail to load.
    pub target: AreaKey,
    /// Where the entity stands after arriving, in the TARGET's coordinates. `x` and `y` are the
    /// area data's; `z` is a placeholder (`0.0`): the target puts the entity on its own walkable
    /// surface ([`AreaRuntime::place_arrival`]).
    pub arrival: Pos,
    /// The heading the entity has on arrival (radians, adapter space), if the area's data says
    /// one. `None` keeps the heading it had when it crossed. FF7 gateways carry no heading (the
    /// arrival field's own script turns the character), so theirs is always `None`.
    pub arrival_facing: Option<f32>,
    /// Adapter-defined hint for placing the arrival on the right walkable surface where several
    /// overlap (FF7: the index of the target field's walkmesh triangle that holds the arrival
    /// point). Handed back to [`AreaRuntime::place_arrival`] of the target area.
    pub arrival_hint: Option<u32>,
    pub kind: ExitKind,
}

impl Exit {
    /// Whether a straight move `from -> to` crosses the exit line, judged on x and y only. The
    /// server asks it of every move; the rule lives here so that a client can predict it.
    ///
    /// A move crosses when it starts strictly on one side of the line's infinite extension and
    /// ends on the other side or exactly on it, and the point where it meets the extension lies on
    /// the segment (its end points count). So standing on the line is never a crossing (an
    /// entity that arrives on one does not bounce straight back), neither is walking along it, and
    /// a move that never reaches it is not one. Non-finite positions never cross, and a line of
    /// zero length (both end points equal) never does either.
    pub fn is_crossed_by(&self, from: Pos, to: Pos) -> bool {
        self.crossing(from, to).is_some()
    }

    /// How far along the move `from -> to` it meets the exit line (`Some(t)` with `t` in `(0, 1]`,
    /// 1 meaning exactly at `to`), or `None` when it does not cross it (see
    /// [`Exit::is_crossed_by`] for the rule). When one move crosses several exits, the one with the
    /// smallest `t` is the one reached first.
    pub fn crossing(&self, from: Pos, to: Pos) -> Option<f64> {
        if !(from.is_finite() && to.is_finite() && self.line.0.is_finite() && self.line.1.is_finite()) {
            return None;
        }
        let (ax, ay) = (f64::from(self.line.0.x), f64::from(self.line.0.y));
        let (bx, by) = (f64::from(self.line.1.x), f64::from(self.line.1.y));
        let (dx, dy) = (bx - ax, by - ay);
        if dx == 0.0 && dy == 0.0 {
            return None;
        }
        let side = |x: f32, y: f32| dx * (f64::from(y) - ay) - dy * (f64::from(x) - ax);
        let (s_from, s_to) = (side(from.x, from.y), side(to.x, to.y));
        // Strictly on one side at the start; on the other side, or on the line, at the end.
        if s_from == 0.0 || (s_to != 0.0 && (s_from > 0.0) == (s_to > 0.0)) {
            return None;
        }
        // Where the move meets the line's extension, as a fraction of the move; and where that is
        // along the segment (0 = first end point, 1 = the second).
        let t = s_from / (s_from - s_to);
        let (mx, my) = (
            f64::from(from.x) + t * (f64::from(to.x) - f64::from(from.x)),
            f64::from(from.y) + t * (f64::from(to.y) - f64::from(from.y)),
        );
        let along = ((mx - ax) * dx + (my - ay) * dy) / (dx * dx + dy * dy);
        (0.0..=1.0).contains(&along).then_some(t)
    }

    /// The middle of the exit line (z is the mean of the end points' heights): a point to steer
    /// toward.
    pub fn midpoint(&self) -> Pos {
        let (a, b) = self.line;
        Pos::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0, (a.z + b.z) / 2.0)
    }
}

/// What a non-player character looks like, in the adapter's own terms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NpcAppearance {
    /// Adapter-defined model reference (FF7: the skeleton stem of the area's model-loader record,
    /// `aaaa`: `<stem>.hrc` in `char.lgp`).
    pub model: String,
    /// The clip that shows the character standing, if the data names one (FF7: the stem of the
    /// first animation of its model-loader record, by the loader's own ordering convention).
    pub idle_animation: Option<String>,
    /// What the area's data calls the model (FF7: the model-loader record's name): for logs and
    /// labels, never for lookups.
    pub label: String,
}

/// A non-player character the area's own data places when the area is set up.
///
/// The server presents these as static, mindless scenery: they stand where the data puts them and
/// face the way it says, and nothing else is simulated.
#[derive(Debug, Clone, PartialEq)]
pub struct NpcSpawn {
    /// The entity's name in the area's data (FF7: the script entity name, `gu0`).
    pub entity_name: String,
    pub appearance: NpcAppearance,
    /// Where it stands, in the area's coordinates (on the walkable surface, as the data says).
    pub pos: Pos,
    /// Heading in radians, adapter space (counter-clockwise from +x, like `EntitySnapshot::facing`).
    pub facing: f32,
    /// The placement runs on every path through the entity's set-up code. `false` means it is
    /// story-dependent: whether the character stands there depends on game variables the server
    /// does not have. The server shows only the unconditional ones.
    pub unconditional: bool,
}

/// What makes the story move in an area, in terms the server can verify for itself.
///
/// Only [`StoryTrigger::Region`] exists. The other things that move an FF7 story (a talk script, a
/// cutscene that runs by itself, a battle that has to be won) need the script VM or the dialogue
/// lane; adding them later means adding a kind.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum StoryTrigger {
    /// The player's character stands in this convex polygon (judged on x and y only, like exits). The
    /// corners are in THIS area's coordinates, in either winding order.
    Region(Vec<Pos>),
}

/// One place where an area's story can advance a player, as its own data defines it. The point says
/// WHERE (the trigger); WHETHER it applies to a player, and what it does to their progression, is
/// [`AreaRuntime::story_advance`]: it depends on the progression, which only the adapter reads.
#[derive(Debug, Clone, PartialEq)]
pub struct StoryPoint {
    /// Identifies the point within its area; stable while the area is hosted. What
    /// [`AreaRuntime::story_advance`] takes.
    pub id: u32,
    pub trigger: StoryTrigger,
    /// A short line for logs: numbers and names of the adapter's own, never text of the game.
    pub label: String,
}

impl StoryPoint {
    /// Whether a move `from -> to` (both in this area's coordinates) touches the trigger: the chord
    /// crosses or ends inside a [`StoryTrigger::Region`]. A move that starts inside it counts too,
    /// so a player that is standing in the region when the point becomes available (its story moved
    /// on while it stood there) is caught by its next step. Non-finite positions never hit.
    ///
    /// The whole chord is tested and not only where it ends, so a fast mover cannot step over a thin
    /// region between two ticks.
    pub fn is_hit_by(&self, from: Pos, to: Pos) -> bool {
        if !(from.is_finite() && to.is_finite()) {
            return false;
        }
        match &self.trigger {
            StoryTrigger::Region(corners) => chord_touches_polygon(corners, from, to),
        }
    }

    /// The middle of the trigger, in x, y and z: a point to steer toward.
    pub fn center(&self) -> Pos {
        match &self.trigger {
            StoryTrigger::Region(corners) => {
                let n = corners.len().max(1) as f32;
                let sum = corners.iter().fold((0.0, 0.0, 0.0), |s, p| (s.0 + p.x, s.1 + p.y, s.2 + p.z));
                Pos::new(sum.0 / n, sum.1 / n, sum.2 / n)
            }
        }
    }
}

/// The result of a [`AreaRuntime::story_advance`]: the progression the player has now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoryStep {
    /// The new progression blob, in the adapter's own encoding (what the server stores for the
    /// account and game).
    pub progress: Vec<u8>,
    /// What changed, as `key=value` words of the adapter's own without spaces, for logs and the
    /// client's log (FF7: `moment=1->6`). Never text of the game.
    pub summary: String,
}

/// Whether the segment `a -> b` meets the convex polygon `corners` (x and y only). An endpoint
/// inside counts, and so does a crossing of any edge. A polygon with fewer than three corners, or
/// with a non-finite corner, is never touched.
fn chord_touches_polygon(corners: &[Pos], a: Pos, b: Pos) -> bool {
    if corners.len() < 3 || corners.iter().any(|p| !p.is_finite()) {
        return false;
    }
    let (ax, ay, bx, by) = (f64::from(a.x), f64::from(a.y), f64::from(b.x), f64::from(b.y));
    // Inside: the point is on the same side (or on) of every edge, whichever way the polygon winds.
    let inside = |x: f64, y: f64| {
        let (mut pos, mut neg) = (false, false);
        for (i, p) in corners.iter().enumerate() {
            let q = corners.get((i + 1) % corners.len()).copied().unwrap_or(*p);
            let cross = (f64::from(q.x) - f64::from(p.x)) * (y - f64::from(p.y)) - (f64::from(q.y) - f64::from(p.y)) * (x - f64::from(p.x));
            pos |= cross > 0.0;
            neg |= cross < 0.0;
        }
        !(pos && neg)
    };
    if inside(ax, ay) || inside(bx, by) {
        return true;
    }
    // Both ends outside: the chord still touches the polygon if it crosses an edge.
    let orient = |px: f64, py: f64, qx: f64, qy: f64, rx: f64, ry: f64| (qx - px) * (ry - py) - (qy - py) * (rx - px);
    corners.iter().enumerate().any(|(i, p)| {
        let q = corners.get((i + 1) % corners.len()).copied().unwrap_or(*p);
        let (px, py, qx, qy) = (f64::from(p.x), f64::from(p.y), f64::from(q.x), f64::from(q.y));
        let (d1, d2) = (orient(ax, ay, bx, by, px, py), orient(ax, ay, bx, by, qx, qy));
        let (d3, d4) = (orient(px, py, qx, qy, ax, ay), orient(px, py, qx, qy, bx, by));
        d1 * d2 <= 0.0 && d3 * d4 <= 0.0 && !(d1 == 0.0 && d2 == 0.0)
    })
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
    /// The ways out of this area (lines the server watches players cross). Additive: the default
    /// is none, which makes the area a dead end. See [`ExitKind`] for what is and is not included.
    fn exits(&self) -> Vec<Exit> {
        Vec::new()
    }
    /// The non-player characters the area's data places (see [`NpcSpawn`]). Additive: the default
    /// is none.
    fn npcs(&self) -> Vec<NpcSpawn> {
        Vec::new()
    }
    /// Where an entity arriving by an [`Exit`] ends up in THIS area: `arrival` snapped onto the
    /// walkable surface (`hint` is the exit's [`Exit::arrival_hint`]). The default settles it with
    /// [`AreaRuntime::validate_move`] from the point to itself, which also moves a point that is
    /// off the surface to where the area would put such an entity (for example its nearest spawn
    /// point). Always a position this area accepts.
    fn place_arrival(&self, arrival: Pos, hint: Option<u32>) -> Pos {
        let _ = hint;
        self.validate_move(arrival, arrival).pos()
    }

    // ---- story: per-account progression, phases and the points that advance it (all additive) ----
    //
    // An account's progression is ONE opaque blob per game (`fmv_persist::Progression`): only the
    // adapter reads it, and an empty blob is a new account (the game's own starting point). The
    // methods below are how the server, which never looks inside the blob, still gets phases and
    // advancement from it. None of them knows who the player is or which character they play: the
    // story belongs to the account, not to the player's look or combat slot.

    /// The story phase of THIS area for an account whose progression is `progress`: the opaque key
    /// that selects which instance of the area it is in (accounts with equal keys share one; party
    /// members are placed by their leader's). `None`: this area does not read the progression (the
    /// default), and the server uses one phase per distinct blob, for every area alike.
    ///
    /// The key for the game's starting progression (the empty blob) must be `PhaseKey::default()`
    /// in every area, so that every existing account starts in one and the same phase and only
    /// advancing the story separates players. Must be cheap: it runs for every placement.
    fn phase_key(&self, progress: &[u8]) -> Option<PhaseKey> {
        let _ = progress;
        None
    }

    /// The places where this area's story can advance a player (see [`StoryPoint`]). The server
    /// watches every accepted move against them. The default is none.
    fn story_points(&self) -> Vec<StoryPoint> {
        Vec::new()
    }

    /// What reaching story point `point` does to an account whose progression is `progress`: the
    /// new progression, or `None` when it does nothing (the point does not apply to that story state,
    /// the point is unknown, or `progress` cannot be read: an unreadable blob is never replaced).
    /// Pure: the server applies the step, saves it and tells the player.
    ///
    /// A story only moves forward: a step never lowers what the progression already records.
    fn story_advance(&self, progress: &[u8], point: u32) -> Option<StoryStep> {
        let _ = (progress, point);
        None
    }

    /// The non-player characters that exist in the instance of this area that runs `phase`. This is
    /// where a phase decides which of the game's cast (and so which events) a player's instance holds.
    /// The default is every NPC the area places ([`AreaRuntime::npcs`]) whatever the phase; the
    /// server shows only those with `unconditional` set, as it always has.
    fn npcs_in_phase(&self, phase: &PhaseKey) -> Vec<NpcSpawn> {
        let _ = phase;
        self.npcs()
    }

    /// The game's CANON party at this story state: the characters of the story that fight beside the
    /// players, as the game's own character ids (what `BattleMember::character` carries), in the order the
    /// story lists them (FF7: Cloud, Barret in the first reactor mission). `progress` is the progression of the
    /// account whose story the party follows (a party's leader). A player's original character is ADDED to this
    /// party, never put in a member's place, and the server commands these members (see `fmv-server`'s
    /// `world::companion`). The default is none: a game whose story has no party of its own fights with
    /// only the players. It must answer for any blob, an unreadable one included (a story that cannot be
    /// read has its game's starting party), and never depends on who the player is or which combat curve
    /// it uses.
    fn story_party(&self, progress: &[u8]) -> Vec<u8> {
        let _ = progress;
        Vec::new()
    }
}

/// A game adapter bound to one owned-content source (or the synthetic fixture).
pub trait GameAdapter: Send + Sync {
    fn game(&self) -> GameId;
    fn list_areas(&self) -> Vec<AreaInfo>;
    fn load_area(&self, key: &str) -> Result<Box<dyn AreaRuntime>, AdapterError>;

    /// Combine the progression an account already has stored with one that was just advanced, into
    /// the progression to store: a story only moves forward, so this keeps whatever either records
    /// (FF7: the further moment). Also what lets a save that arrives late (a player that logged out and
    /// in again before it was written) never take a stored story back. A blob the adapter cannot read
    /// must come back unchanged if it is `stored` (an unreadable record is never overwritten). The
    /// default is `advanced`: the newest write wins.
    fn merge_progression(&self, stored: &[u8], advanced: &[u8]) -> Vec<u8> {
        let _ = stored;
        advanced.to_vec()
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

    fn exit(a: (f32, f32), b: (f32, f32)) -> Exit {
        Exit {
            line: (Pos::new(a.0, a.1, 5.0), Pos::new(b.0, b.1, 7.0)),
            target: "next".to_string(),
            arrival: Pos::default(),
            arrival_facing: None,
            arrival_hint: None,
            kind: ExitKind::Gateway,
        }
    }

    fn at(x: f32, y: f32) -> Pos {
        Pos::new(x, y, 123.0)
    }

    #[test]
    fn a_move_across_the_exit_line_crosses_it_and_nothing_else_does() {
        let e = exit((0.0, 0.0), (10.0, 0.0)); // the x axis from 0 to 10
        // straight across, either way, anywhere along the segment (end points included)
        assert!(e.is_crossed_by(at(5.0, 1.0), at(5.0, -1.0)));
        assert!(e.is_crossed_by(at(5.0, -1.0), at(5.0, 1.0)));
        assert!(e.is_crossed_by(at(0.0, 1.0), at(0.0, -1.0)), "the first end point is on the line");
        assert!(e.is_crossed_by(at(10.0, 1.0), at(10.0, -1.0)), "so is the second");
        assert!(e.is_crossed_by(at(2.0, 3.0), at(8.0, -3.0)), "a slanted crossing");
        // reaching the line exactly counts, because the move got there
        assert!(e.is_crossed_by(at(5.0, 2.0), at(5.0, 0.0)));
        // past the segment's ends, along the extension, is not on the exit
        assert!(!e.is_crossed_by(at(11.0, 1.0), at(11.0, -1.0)));
        assert!(!e.is_crossed_by(at(-0.5, 1.0), at(-0.5, -1.0)));
        assert!(!e.is_crossed_by(at(-5.0, 3.0), at(-1.0, -3.0)), "meets the extension at x = -3, off the segment");
        assert!(e.is_crossed_by(at(-5.0, 3.0), at(15.0, -3.0)), "meets it at x = 5, on the segment");
        // a move that stays on one side, or stops short of the line, is no crossing
        assert!(!e.is_crossed_by(at(5.0, 3.0), at(5.0, 1.0)));
        assert!(!e.is_crossed_by(at(5.0, -3.0), at(7.0, -1.0)));
        // standing on the line is not crossing it, and neither is leaving it or walking along it
        assert!(!e.is_crossed_by(at(5.0, 0.0), at(5.0, 0.0)));
        assert!(!e.is_crossed_by(at(5.0, 0.0), at(5.0, -2.0)), "an entity that arrives on the line does not bounce back");
        assert!(!e.is_crossed_by(at(5.0, 0.0), at(5.0, 2.0)));
        assert!(!e.is_crossed_by(at(2.0, 0.0), at(8.0, 0.0)));
        // heights play no part
        assert!(e.is_crossed_by(Pos::new(5.0, 1.0, -1000.0), Pos::new(5.0, -1.0, 1000.0)));
        // the fraction says how far along the move the line was met (so the first of two exits
        // can be told from the second)
        assert_eq!(e.crossing(at(5.0, 1.0), at(5.0, -1.0)), Some(0.5));
        assert_eq!(e.crossing(at(5.0, 1.0), at(5.0, 0.0)), Some(1.0), "reaching it at the very end");
        assert_eq!(e.crossing(at(5.0, 4.0), at(5.0, -12.0)), Some(0.25));
        assert_eq!(e.crossing(at(5.0, 1.0), at(5.0, 3.0)), None);
    }

    #[test]
    fn crossing_is_never_claimed_for_garbage() {
        let e = exit((0.0, 0.0), (10.0, 0.0));
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(!e.is_crossed_by(at(bad, 1.0), at(5.0, -1.0)));
            assert!(!e.is_crossed_by(at(5.0, 1.0), at(5.0, bad)));
        }
        let nan_line = exit((f32::NAN, 0.0), (10.0, 0.0));
        assert!(!nan_line.is_crossed_by(at(5.0, 1.0), at(5.0, -1.0)));
        let point = exit((3.0, 3.0), (3.0, 3.0));
        assert!(!point.is_crossed_by(at(3.0, 4.0), at(3.0, 2.0)), "a line of zero length is not an exit");
        // a vertical and a slanted line behave like the horizontal one
        let v = exit((4.0, -5.0), (4.0, 5.0));
        assert!(v.is_crossed_by(at(3.0, 0.0), at(5.0, 0.0)) && v.is_crossed_by(at(5.0, 0.0), at(3.0, 0.0)));
        assert!(!v.is_crossed_by(at(3.0, 6.0), at(5.0, 6.0)));
        let slanted = exit((0.0, 0.0), (10.0, 10.0));
        assert!(slanted.is_crossed_by(at(6.0, 4.0), at(4.0, 6.0)));
        assert!(!slanted.is_crossed_by(at(6.0, 4.0), at(9.0, 6.0)));
        assert_eq!(slanted.midpoint(), Pos::new(5.0, 5.0, 6.0));
    }

    #[test]
    fn the_new_area_hooks_default_to_nothing_and_arrivals_settle_through_validate_move() {
        struct Flat(AreaInfo);
        impl AreaRuntime for Flat {
            fn info(&self) -> &AreaInfo {
                &self.0
            }
            fn spawn_points(&self) -> Vec<Pos> {
                vec![Pos::new(1.0, 1.0, 9.0)]
            }
            fn validate_move(&self, from: Pos, to: Pos) -> MoveResult {
                // a floor at height 9 for x >= 0; everything else is off it (back to the spawn)
                if to.x >= 0.0 && to.is_finite() {
                    MoveResult::Accepted(Pos::new(to.x, to.y, 9.0))
                } else if from.x >= 0.0 {
                    MoveResult::Corrected(from)
                } else {
                    MoveResult::Corrected(self.spawn_points()[0])
                }
            }
            fn bounds(&self) -> Bounds {
                Bounds { min: Pos::default(), max: Pos::default() }
            }
            fn walk_triangles(&self) -> Vec<[Pos; 3]> {
                Vec::new()
            }
            fn max_speed(&self) -> f32 {
                1.0
            }
        }
        let info = AreaInfo {
            id: fmv_core::AreaId { game: GameId::Fixture, key: "flat".into() },
            display_name: "flat".into(),
            fingerprint: Fingerprint([0; 32]),
            provenance: EditionProvenance {
                game: GameId::Fixture,
                platform: fmv_core::Platform::Synthetic,
                edition: "t".into(),
                region: "t".into(),
                language: "t".into(),
                revision: "t".into(),
                sources: vec![],
                modded: false,
            },
            fixture: true,
        };
        let area = Flat(info);
        assert!(area.exits().is_empty() && area.npcs().is_empty() && area.encounters().is_none());
        assert_eq!(area.place_arrival(Pos::new(3.0, 4.0, 0.0), Some(12)), Pos::new(3.0, 4.0, 9.0), "snapped to the floor");
        assert_eq!(area.place_arrival(Pos::new(-3.0, 4.0, 0.0), None), Pos::new(1.0, 1.0, 9.0), "off the floor: the area's own choice");
    }

    fn region(corners: &[(f32, f32)]) -> StoryPoint {
        StoryPoint {
            id: 7,
            trigger: StoryTrigger::Region(corners.iter().map(|&(x, y)| Pos::new(x, y, 3.0)).collect()),
            label: "test".into(),
        }
    }

    #[test]
    fn a_chord_that_touches_the_region_hits_it_however_it_gets_there() {
        // a right triangle, once wound each way
        for corners in [[(0.0, 0.0), (10.0, 0.0), (0.0, 10.0)], [(0.0, 0.0), (0.0, 10.0), (10.0, 0.0)]] {
            let p = region(&corners);
            // ends inside, starts inside, stays inside
            assert!(p.is_hit_by(at(20.0, 20.0), at(2.0, 2.0)));
            assert!(p.is_hit_by(at(2.0, 2.0), at(20.0, 20.0)));
            assert!(p.is_hit_by(at(1.0, 1.0), at(2.0, 2.0)));
            // both ends outside, the chord goes through it (a fast mover between two ticks)
            assert!(p.is_hit_by(at(-5.0, 3.0), at(15.0, 3.0)));
            assert!(p.is_hit_by(at(3.0, -5.0), at(3.0, 15.0)));
            // both ends outside and it only clips a corner
            assert!(p.is_hit_by(at(-1.0, 2.0), at(2.0, -1.0)));
            // misses: beside it, short of it, past the hypotenuse
            assert!(!p.is_hit_by(at(-5.0, -5.0), at(-1.0, 20.0)));
            assert!(!p.is_hit_by(at(11.0, -5.0), at(11.0, 5.0)));
            assert!(!p.is_hit_by(at(-5.0, 5.0), at(-1.0, 5.0)));
            assert!(!p.is_hit_by(at(8.0, 8.0), at(20.0, 20.0)), "(8, 8) is past the hypotenuse x + y = 10");
            // heights play no part, and a corner or an edge counts as inside
            assert!(p.is_hit_by(Pos::new(2.0, 2.0, -999.0), Pos::new(2.0, 2.0, 999.0)));
            assert!(p.is_hit_by(at(5.0, 5.0), at(5.0, 5.0)), "on the hypotenuse");
            assert!(p.is_hit_by(at(0.0, 0.0), at(0.0, 0.0)), "on a corner");
        }
    }

    #[test]
    fn a_region_is_never_hit_by_garbage_or_by_a_degenerate_polygon() {
        let p = region(&[(0.0, 0.0), (10.0, 0.0), (0.0, 10.0)]);
        for bad in [f32::NAN, f32::INFINITY] {
            assert!(!p.is_hit_by(at(bad, 2.0), at(2.0, 2.0)));
            assert!(!p.is_hit_by(at(2.0, 2.0), at(2.0, bad)));
        }
        assert!(!region(&[(0.0, 0.0), (10.0, 0.0)]).is_hit_by(at(1.0, 0.0), at(2.0, 0.0)), "two corners are no region");
        assert!(!region(&[(0.0, 0.0), (f32::NAN, 0.0), (0.0, 10.0)]).is_hit_by(at(1.0, 1.0), at(2.0, 2.0)));
        assert_eq!(p.center(), Pos::new(10.0 / 3.0, 10.0 / 3.0, 3.0));
    }

    #[test]
    fn the_story_hooks_default_to_a_story_that_does_nothing() {
        struct Plain(AreaInfo);
        impl AreaRuntime for Plain {
            fn info(&self) -> &AreaInfo {
                &self.0
            }
            fn spawn_points(&self) -> Vec<Pos> {
                Vec::new()
            }
            fn validate_move(&self, from: Pos, _to: Pos) -> MoveResult {
                MoveResult::Corrected(from)
            }
            fn bounds(&self) -> Bounds {
                Bounds { min: Pos::default(), max: Pos::default() }
            }
            fn walk_triangles(&self) -> Vec<[Pos; 3]> {
                Vec::new()
            }
            fn max_speed(&self) -> f32 {
                1.0
            }
        }
        let info = AreaInfo {
            id: fmv_core::AreaId { game: GameId::Fixture, key: "plain".into() },
            display_name: "plain".into(),
            fingerprint: Fingerprint([0; 32]),
            provenance: EditionProvenance {
                game: GameId::Fixture,
                platform: fmv_core::Platform::Synthetic,
                edition: "t".into(),
                region: "t".into(),
                language: "t".into(),
                revision: "t".into(),
                sources: vec![],
                modded: false,
            },
            fixture: true,
        };
        let area = Plain(info);
        assert_eq!(area.phase_key(&[1, 2, 3]), None, "no opinion: the server uses one phase per distinct blob");
        assert!(area.story_points().is_empty());
        assert_eq!(area.story_advance(&[], 0), None);
        assert!(area.npcs_in_phase(&PhaseKey(vec![9])).is_empty());
        assert!(area.story_party(&[1, 2, 3]).is_empty(), "no canon party: the players fight alone");
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
