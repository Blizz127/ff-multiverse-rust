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
/// A [`ExitKind::Gateway`] is data and always open. A [`ExitKind::Script`] is a way out that a script
/// decides (FF7: a `MAPJUMP` that a line's trigger script runs when the player walks onto the line): whether
/// it is open depends on the game's story variables, which only the adapter reads
/// ([`AreaRuntime::exit_state`]). The other ways out of an FF7 field (a `MAPJUMP` a talk script runs, a
/// cut-scene's director) are not walk-on exits and are not reported; adding them later means adding a kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExitKind {
    /// A gateway of the area's own data (FF7: one of the twelve gateways of the field's trigger
    /// section): walking across its line leads straight to another area, no script involved.
    Gateway,
    /// An exit a script decides. `id` is the adapter's own number for it, handed back to
    /// [`AreaRuntime::exit_state`]. Several script exits may share one line (a script that picks the
    /// destination from the story): at most one of them is open for a given progression.
    Script { id: u32 },
}

/// Whether an exit is open for an account's story ([`AreaRuntime::exit_state`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExitState {
    /// Crossing the line leads to the target.
    Open,
    /// The exit's conditions say no for this story (it belongs to another moment of it).
    Closed,
    /// A condition of the exit reads something the adapter cannot evaluate (a variable the progression does
    /// not carry, a key the player has to press): the exit stays closed. The text says what, in numbers and
    /// the adapter's own words, never text of the game; the server reports it once per area.
    Unknown(String),
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
        line_crossing(self.line, from, to)
    }

    /// The middle of the exit line (z is the mean of the end points' heights): a point to steer
    /// toward.
    pub fn midpoint(&self) -> Pos {
        let (a, b) = self.line;
        Pos::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0, (a.z + b.z) / 2.0)
    }

    /// Whether `other` is the same line as this exit's (the same two end points on x and y, in either order; the
    /// heights play no part in a crossing): the several script exits of one trigger line share it.
    pub fn same_line_as(&self, other: &Exit) -> bool {
        let xy = |p: Pos| (p.x, p.y);
        let (a, b) = (xy(self.line.0), xy(self.line.1));
        let (c, d) = (xy(other.line.0), xy(other.line.1));
        (a == c && b == d) || (a == d && b == c)
    }
}

/// How far along the move `from -> to` it meets the line segment `line` (`Some(t)` with `t` in `(0, 1]`),
/// or `None` when it does not cross it. The one crossing rule of exits ([`Exit::crossing`], which says why it
/// is what it is) and of line triggers ([`StoryTrigger::Line`]).
fn line_crossing(line: (Pos, Pos), from: Pos, to: Pos) -> Option<f64> {
    if !(from.is_finite() && to.is_finite() && line.0.is_finite() && line.1.is_finite()) {
        return None;
    }
    let (ax, ay) = (f64::from(line.0.x), f64::from(line.0.y));
    let (bx, by) = (f64::from(line.1.x), f64::from(line.1.y));
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

/// A line that puts a player somewhere else in the SAME area when the player reaches it: a ladder, a jump over a
/// gap (FF7: a line entity whose script moves the player's character with `JUMP` or `LADER`). The server does the
/// move itself and tells the client with `MoveRejected { corrected }`, the message that already puts a player
/// where the server says it is, so the protocol needs nothing for it.
///
/// A link fires in one of two ways. A link that **needs OK** ([`Link::needs_ok`]: the script waits for the OK
/// button on the line) fires when the player presses OK (`ClientMsg::Interact`) while touching the line
/// ([`Link::is_touched_at`]). Any other link is a **walk-on** link, which the script fires with no key: it fires
/// when the player walks up to the line, but only while it is *armed*: see [`LinkArming`] (a player that arrives
/// next to a line must not bounce straight back over it).
///
/// Lines have a height: the line of a ladder at the bottom of a stair is not under the feet of a player walking
/// over it on the level above, whatever x and y say ([`Link::on_level`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    /// End points of the line in THIS area's coordinates; z is the height of the level the line is on.
    pub line: (Pos, Pos),
    /// Where the player stands afterwards. `x` and `y` are the data's, `z` a placeholder: the area puts the
    /// player on its own surface ([`AreaRuntime::place_arrival`] with [`Link::to_hint`]).
    pub to: Pos,
    /// Hint for placing the destination on the right level where several overlap (see [`Exit::arrival_hint`]).
    pub to_hint: Option<u32>,
    /// Whether the line waits for the OK button.
    pub needs_ok: bool,
    /// A short word for logs (`jump`, `ladder`), never text of the game.
    pub label: String,
}

impl Link {
    /// How near (x and y) a player has to be to the line to touch it.
    pub const TOUCH: f32 = 12.0;
    /// How far from the line a player has to be to have left it (and arm a walk-on link again).
    pub const REACH: f32 = 30.0;
    /// How far a player's height may be from the line's for the line to be on the player's level.
    pub const LEVEL: f32 = 80.0;

    /// The distance on x and y from `p` to the line (a segment).
    pub fn distance_xy(&self, p: Pos) -> f32 {
        point_segment_distance(p, self.line.0, self.line.1)
    }

    /// The height of the line at the point nearest to `p`.
    fn height_near(&self, p: Pos) -> f32 {
        let (a, b) = self.line;
        let (dx, dy) = (f64::from(b.x - a.x), f64::from(b.y - a.y));
        let len2 = dx * dx + dy * dy;
        let t = if len2 > 0.0 { ((f64::from(p.x - a.x) * dx + f64::from(p.y - a.y) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
        a.z + (b.z - a.z) * t as f32
    }

    /// Whether a player at `p` is on the level of the line.
    pub fn on_level(&self, p: Pos) -> bool {
        p.is_finite() && (p.z - self.height_near(p)).abs() <= Self::LEVEL
    }

    /// Whether a player standing at `p` touches the line: within [`Link::TOUCH`] of it on x and y, and on its level.
    pub fn is_touched_at(&self, p: Pos) -> bool {
        p.is_finite() && self.line.0.is_finite() && self.line.1.is_finite() && self.distance_xy(p) <= Self::TOUCH && self.on_level(p)
    }

    /// Whether a move `from -> to` touches the line: its chord comes within [`Link::TOUCH`] of it on x and y (a fast
    /// mover does not step over a line between two ticks), and the player ends on the line's level.
    pub fn is_touched_by(&self, from: Pos, to: Pos) -> bool {
        from.is_finite()
            && to.is_finite()
            && self.line.0.is_finite()
            && self.line.1.is_finite()
            && segment_distance((from, to), self.line) <= Self::TOUCH
            && self.on_level(to)
    }

    /// The middle of the line (z the mean of the end points'): a point to steer toward to touch it.
    pub fn touch_point(&self) -> Pos {
        let (a, b) = self.line;
        Pos::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0, (a.z + b.z) / 2.0)
    }
}

/// Which walk-on links a player can fire: the edge trigger of a line that no key fires. A link is **armed** while
/// the player is farther than [`Link::REACH`] from its line, and fires when the player's move touches it
/// ([`Link::is_touched_by`]) while it is armed. A player placed anywhere (it joined, arrived through an exit, or was
/// moved by a link) starts with every link within reach DISARMED, and a disarmed link arms again only when the player
/// has been farther than [`Link::REACH`] from its line. So a ladder that puts a player down on the very line of the
/// ladder back up (the stair of `nmkin_4` does, within a unit) does not send it straight back, and a player must
/// leave a line before it fires again. The server and a bot keep the same state from the same positions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LinkArming {
    disarmed: Vec<bool>,
}

impl LinkArming {
    /// The state of a player just placed at `at`: every link within reach disarmed.
    pub fn placed_at(links: &[Link], at: Pos) -> Self {
        Self { disarmed: links.iter().map(|l| !at.is_finite() || l.distance_xy(at) <= Link::REACH).collect() }
    }

    /// Whether link `index` is armed (an index the state does not know is not).
    pub fn is_armed(&self, index: usize) -> bool {
        self.disarmed.get(index).is_some_and(|disarmed| !disarmed)
    }

    /// Account for the player's move `from -> to`: the index of the walk-on link (not one that needs OK) the move
    /// fires, if one is armed and touched (the first in the list when several are), and the links the player is now
    /// farther than [`Link::REACH`] from armed again. A player that fires a link is placed anew by the caller
    /// ([`LinkArming::placed_at`]).
    pub fn step(&mut self, links: &[Link], from: Pos, to: Pos) -> Option<usize> {
        if self.disarmed.len() != links.len() {
            *self = Self::placed_at(links, from);
        }
        let fired = links.iter().enumerate().find(|(i, l)| !l.needs_ok && self.is_armed(*i) && l.is_touched_by(from, to)).map(|(i, _)| i);
        for (disarmed, link) in self.disarmed.iter_mut().zip(links) {
            if *disarmed && to.is_finite() && link.distance_xy(to) > Link::REACH {
                *disarmed = false;
            }
        }
        fired
    }
}

/// The distance on x and y from point `p` to the segment `a`-`b`.
fn point_segment_distance(p: Pos, a: Pos, b: Pos) -> f32 {
    let (px, py, ax, ay, bx, by) = (f64::from(p.x), f64::from(p.y), f64::from(a.x), f64::from(a.y), f64::from(b.x), f64::from(b.y));
    let (dx, dy) = (bx - ax, by - ay);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 { (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
    (px - (ax + t * dx)).hypot(py - (ay + t * dy)) as f32
}

/// The least distance on x and y between the segments `p` and `q` (0 when they cross or touch).
fn segment_distance(p: (Pos, Pos), q: (Pos, Pos)) -> f32 {
    let cross = |o: Pos, a: Pos, b: Pos| {
        (f64::from(a.x) - f64::from(o.x)) * (f64::from(b.y) - f64::from(o.y)) - (f64::from(a.y) - f64::from(o.y)) * (f64::from(b.x) - f64::from(o.x))
    };
    let (d1, d2) = (cross(q.0, q.1, p.0), cross(q.0, q.1, p.1));
    let (d3, d4) = (cross(p.0, p.1, q.0), cross(p.0, p.1, q.1));
    if d1 * d2 < 0.0 && d3 * d4 < 0.0 {
        return 0.0;
    }
    [point_segment_distance(p.0, q.0, q.1), point_segment_distance(p.1, q.0, q.1), point_segment_distance(q.0, p.0, p.1), point_segment_distance(q.1, p.0, p.1)]
        .into_iter()
        .fold(f32::INFINITY, f32::min)
}

/// What makes the story move in an area, in terms the server can verify for itself.
///
/// The things the server can see are where a player walks ([`StoryTrigger::Region`], [`StoryTrigger::Line`])
/// and that it came to stand in the area ([`StoryTrigger::Enter`]). The other things that move an FF7 story
/// (a talk script, a battle that has to be won) need an interaction the protocol does not carry, the script
/// VM or the dialogue lane; adding them later means adding a kind.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum StoryTrigger {
    /// The player's character stands in this convex polygon (judged on x and y only, like exits). The
    /// corners are in THIS area's coordinates, in either winding order.
    Region(Vec<Pos>),
    /// The player's character walks across this line (the rule of [`Exit::crossing`]: strictly from one
    /// side to the other, or onto it), in THIS area's coordinates.
    Line(Pos, Pos),
    /// The player has just come to stand in the area (it joined, or arrived through an exit): what an area's
    /// own scripts do by themselves from the first frame on (an FF7 field's characters walking about and
    /// moving the story along). Never touched by a move.
    Enter,
    /// The player PRESSES OK (the protocol's `ClientMsg::Interact`) while its character stands in this convex
    /// polygon (x and y only, in THIS area's coordinates): what a script that waits for the OK button on a spot
    /// does (FF7: the elevator's panel). Never touched by a move.
    Press(Vec<Pos>),
    /// The player presses OK next to the character that stands at `center`, within `radius` of it (x and y, and
    /// the character's own level): what a character's talk script does (FF7: a character whose talk moves the
    /// story). Never touched by a move.
    Talk { center: Pos, radius: f32 },
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
    /// crosses or ends inside a [`StoryTrigger::Region`], or crosses a [`StoryTrigger::Line`] (see
    /// [`Exit::crossing`]; standing on a line is not crossing it). A move that starts inside a region counts
    /// too, so a player that is standing in the region when the point becomes available (its story moved
    /// on while it stood there) is caught by its next step. A [`StoryTrigger::Enter`] is never touched by a
    /// move. Non-finite positions never hit.
    ///
    /// The whole chord is tested and not only where it ends, so a fast mover cannot step over a thin
    /// region between two ticks.
    pub fn is_hit_by(&self, from: Pos, to: Pos) -> bool {
        if !(from.is_finite() && to.is_finite()) {
            return false;
        }
        match &self.trigger {
            StoryTrigger::Region(corners) => chord_touches_polygon(corners, from, to),
            StoryTrigger::Line(a, b) => line_crossing((*a, *b), from, to).is_some(),
            StoryTrigger::Enter | StoryTrigger::Press(_) | StoryTrigger::Talk { .. } => false,
        }
    }

    /// Whether the point fires on an OK press ([`StoryTrigger::Press`], [`StoryTrigger::Talk`]).
    pub fn is_on_press(&self) -> bool {
        matches!(self.trigger, StoryTrigger::Press(_) | StoryTrigger::Talk { .. })
    }

    /// Whether a player that stands at `at` and presses OK reaches the point: inside a [`StoryTrigger::Press`]
    /// region, or within a [`StoryTrigger::Talk`]'s radius of its character on the character's level.
    /// `addressed` is where the character the client says it addressed stands, when it names one: it has to be
    /// the character of the point (within [`SAME_SPOT`] of its centre), or the press is not for this point.
    /// Other triggers are never pressed.
    pub fn is_pressed_at(&self, at: Pos, addressed: Option<Pos>) -> bool {
        if !at.is_finite() {
            return false;
        }
        match &self.trigger {
            StoryTrigger::Press(corners) => chord_touches_polygon(corners, at, at),
            StoryTrigger::Talk { center, radius } => {
                center.is_finite()
                    && at.dist_xy(*center) <= *radius
                    && (at.z - center.z).abs() <= Link::LEVEL
                    && addressed.is_none_or(|spot| spot.is_finite() && spot.dist_xy(*center) <= SAME_SPOT)
            }
            _ => false,
        }
    }

    /// Whether the point fires when the player comes to stand in the area ([`StoryTrigger::Enter`]).
    pub fn is_on_entry(&self) -> bool {
        matches!(self.trigger, StoryTrigger::Enter)
    }

    /// The middle of the trigger, in x, y and z: a point to steer toward (the origin for an
    /// [`StoryTrigger::Enter`], which is nowhere).
    pub fn center(&self) -> Pos {
        match &self.trigger {
            StoryTrigger::Region(corners) => {
                let n = corners.len().max(1) as f32;
                let sum = corners.iter().fold((0.0, 0.0, 0.0), |s, p| (s.0 + p.x, s.1 + p.y, s.2 + p.z));
                Pos::new(sum.0 / n, sum.1 / n, sum.2 / n)
            }
            StoryTrigger::Line(a, b) => Pos::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0, (a.z + b.z) / 2.0),
            StoryTrigger::Press(corners) => {
                let n = corners.len().max(1) as f32;
                let sum = corners.iter().fold((0.0, 0.0, 0.0), |s, p| (s.0 + p.x, s.1 + p.y, s.2 + p.z));
                Pos::new(sum.0 / n, sum.1 / n, sum.2 / n)
            }
            StoryTrigger::Talk { center, .. } => *center,
            StoryTrigger::Enter => Pos::default(),
        }
    }
}

/// How near (x and y) the character a client says it addressed has to stand to the centre of a
/// [`StoryTrigger::Talk`] for the press to be for that point: the same spot, to the unit.
pub const SAME_SPOT: f32 = 2.0;

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

/// What an area's data holds of the story (exits its scripts decide, points that move the story) that its adapter
/// does NOT offer, and why: see [`AreaRuntime::story_withheld`]. Descriptions are the adapter's own, in words and
/// numbers of its own (never text of the game); the server logs them once per area.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StoryWithheld {
    /// Why they are left off, as a short phrase (FF7: `unreviewed field`).
    pub reason: String,
    /// The exits left off, one description each (FF7: the field they lead to and the conditions they sit under).
    pub exits: Vec<String>,
    /// The links left off ([`Link`]: ladders and jumps inside the area), one description each.
    pub links: Vec<String>,
    /// The story points left off, one label each (the labels [`StoryPoint::label`] would have had).
    pub points: Vec<String>,
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

    /// Whether `exit` (one of [`AreaRuntime::exits`]) is open for the story `progress` (an account's progression
    /// blob, as for [`AreaRuntime::phase_key`]): the server asks it of every exit a move crosses, with the
    /// progression of the account that governs the crossing player's instance (the party leader's, for a party
    /// member), and takes the first exit that answers [`ExitState::Open`]; a player that crosses a line whose exits
    /// are all closed walks over it as over any line. The default is open (a gateway is data), so only an adapter
    /// that reports [`ExitKind::Script`] exits needs to answer.
    fn exit_state(&self, exit: &Exit, progress: &[u8]) -> ExitState {
        let _ = (exit, progress);
        ExitState::Open
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

    /// The ways inside this area that put a player somewhere else in it (see [`Link`]): ladders, jumps over gaps.
    /// Additive: the default is none.
    fn links(&self) -> Vec<Link> {
        Vec::new()
    }

    /// The battle that reaching story point `point` starts first, if it starts one (an FF7 script that runs a
    /// `BATTLE` before it moves the story): the formation id (what `EncounterStart::formation_id` names). The
    /// server starts that battle for the player's party instead of applying the step, and applies
    /// [`AreaRuntime::story_advance`]'s step when the battle is WON (judged again on the story the player has
    /// then); a lost or fled battle leaves the story where it was. `None` (the default): the step applies at once.
    fn story_battle(&self, point: u32) -> Option<u16> {
        let _ = point;
        None
    }

    /// The exits and story points this area's data holds that the adapter chose not to offer (neither in
    /// [`AreaRuntime::exits`] nor in [`AreaRuntime::story_points`]), and why: `None` (the default) when nothing was
    /// left off. An adapter whose story data has not been reviewed for every area keeps it off where it has not been
    /// (a point raises an account's story for good, and an exit sends a party somewhere): the server logs what it
    /// is told here once per area, so that the owner can see what the world is not doing.
    fn story_withheld(&self) -> Option<StoryWithheld> {
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
        assert_eq!(area.story_withheld(), None, "by default nothing is left off");
        assert!(area.links().is_empty() && area.story_battle(0).is_none(), "no links and no battles by default");
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

    fn line_point(a: (f32, f32), b: (f32, f32)) -> StoryPoint {
        StoryPoint { id: 3, trigger: StoryTrigger::Line(Pos::new(a.0, a.1, 1.0), Pos::new(b.0, b.1, 3.0)), label: "line".into() }
    }

    #[test]
    fn a_line_trigger_is_hit_by_exactly_the_moves_that_would_cross_an_exit_on_it() {
        let (a, b) = ((0.0, 0.0), (10.0, 0.0));
        let p = line_point(a, b);
        let e = exit(a, b);
        // the same rule as an exit's, over a grid of chords
        for (fx, fy, tx, ty) in [
            (5.0, 1.0, 5.0, -1.0),
            (5.0, -1.0, 5.0, 1.0),
            (5.0, 2.0, 5.0, 0.0),
            (5.0, 0.0, 5.0, -2.0),
            (5.0, 0.0, 5.0, 5.0),
            (11.0, 1.0, 11.0, -1.0),
            (-5.0, 3.0, 15.0, -3.0),
            (2.0, 0.0, 8.0, 0.0),
            (5.0, 3.0, 5.0, 1.0),
        ] {
            assert_eq!(p.is_hit_by(at(fx, fy), at(tx, ty)), e.is_crossed_by(at(fx, fy), at(tx, ty)), "{fx},{fy} -> {tx},{ty}");
        }
        assert!(p.is_hit_by(at(5.0, 1.0), at(5.0, -1.0)) && !p.is_hit_by(at(5.0, 1.0), at(5.0, 3.0)));
        assert!(!p.is_hit_by(at(5.0, 0.0), at(5.0, 0.0)), "standing on a line is not walking over it, unlike a region");
        assert_eq!(p.center(), Pos::new(5.0, 0.0, 2.0));
        assert!(!p.is_on_entry());
        // garbage never hits
        assert!(!p.is_hit_by(at(f32::NAN, 1.0), at(5.0, -1.0)) && !p.is_hit_by(at(5.0, 1.0), at(5.0, f32::INFINITY)));
    }

    #[test]
    fn an_entry_trigger_is_never_touched_by_a_move_and_says_it_fires_on_entry() {
        let p = StoryPoint { id: 0, trigger: StoryTrigger::Enter, label: "enter".into() };
        assert!(p.is_on_entry());
        for (from, to) in [(at(0.0, 0.0), at(0.0, 0.0)), (at(-100.0, 0.0), at(100.0, 0.0))] {
            assert!(!p.is_hit_by(from, to));
        }
        assert!(!region(&[(0.0, 0.0), (1.0, 0.0), (0.0, 1.0)]).is_on_entry());
        assert_eq!(p.center(), Pos::default());
    }

    #[test]
    fn script_exits_share_a_line_in_either_direction_and_every_exit_is_open_by_default() {
        let one = exit((0.0, 0.0), (10.0, 0.0));
        let mut other = exit((10.0, 0.0), (0.0, 0.0));
        other.kind = ExitKind::Script { id: 4 };
        assert!(one.same_line_as(&other) && other.same_line_as(&one), "the same two points, either way round");
        assert!(!one.same_line_as(&exit((0.0, 0.0), (10.0, 1.0))));
        assert_ne!(one.kind, other.kind);
        // a plain adapter answers open for every exit of any kind (a gateway is data)
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
        assert_eq!(area.exit_state(&one, &[]), ExitState::Open);
        assert_eq!(area.exit_state(&other, &[9, 9]), ExitState::Open);
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

    /// A link along x = `x` from y = -50 to 50 at height `z`.
    fn link_at(x: f32, z: f32, needs_ok: bool) -> Link {
        Link { line: (Pos::new(x, -50.0, z), Pos::new(x, 50.0, z)), to: Pos::new(500.0, 0.0, 0.0), to_hint: Some(3), needs_ok, label: "jump".into() }
    }

    #[test]
    fn a_link_is_touched_within_a_few_units_of_its_line_and_on_its_level_only() {
        let link = link_at(0.0, 100.0, false);
        assert_eq!(link.distance_xy(Pos::new(8.0, 0.0, 100.0)), 8.0);
        assert_eq!(link.distance_xy(Pos::new(0.0, 80.0, 100.0)), 30.0, "past the end of the segment: the distance to the end point");
        assert!(link.is_touched_at(Pos::new(Link::TOUCH, 10.0, 100.0)));
        assert!(!link.is_touched_at(Pos::new(Link::TOUCH + 0.5, 10.0, 100.0)));
        assert!(!link.is_touched_at(Pos::new(0.0, 70.0, 100.0)), "beyond the end of the line");
        // the level: x and y say it is under the player's feet, the height says it is another floor
        assert!(link.is_touched_at(Pos::new(0.0, 0.0, 100.0 + Link::LEVEL)));
        assert!(!link.is_touched_at(Pos::new(0.0, 0.0, 100.0 + Link::LEVEL + 1.0)));
        assert!(!link.is_touched_at(Pos::new(0.0, 0.0, -360.0)), "the level below: 460 units down");
        // a line that slopes: its height at the nearest point
        let ramp = Link { line: (Pos::new(0.0, -50.0, 0.0), Pos::new(0.0, 50.0, 400.0)), ..link.clone() };
        assert!(ramp.is_touched_at(Pos::new(0.0, -50.0, 40.0)) && !ramp.is_touched_at(Pos::new(0.0, -50.0, 200.0)));
        assert!(ramp.is_touched_at(Pos::new(0.0, 50.0, 380.0)) && !ramp.is_touched_at(Pos::new(0.0, 50.0, 0.0)));
        // non-finite positions touch nothing
        assert!(!link.is_touched_at(Pos::new(f32::NAN, 0.0, 100.0)) && !link.is_touched_by(Pos::new(0.0, 0.0, 100.0), Pos::new(f32::INFINITY, 0.0, 100.0)));
        assert_eq!(link.touch_point(), Pos::new(0.0, 0.0, 100.0));
    }

    #[test]
    fn a_move_touches_a_link_when_its_chord_comes_near_the_line_even_if_it_ends_far_beyond_it() {
        let link = link_at(0.0, 0.0, false);
        let p = |x: f32, y: f32| Pos::new(x, y, 0.0);
        assert!(link.is_touched_by(p(-40.0, 0.0), p(40.0, 0.0)), "across the line: a fast mover does not step over it");
        assert!(link.is_touched_by(p(-40.0, 0.0), p(-5.0, 0.0)), "up to it");
        assert!(!link.is_touched_by(p(-40.0, 0.0), p(-20.0, 0.0)), "short of it");
        assert!(!link.is_touched_by(p(-40.0, 70.0), p(40.0, 70.0)), "across the line's extension, 20 units beyond the end of the segment");
        assert!(link.is_touched_by(p(-40.0, 55.0), p(40.0, 55.0)), "5 units beyond the end of the segment is still touching it");
        // the level counts at the end of the move
        assert!(!link.is_touched_by(p(-40.0, 0.0), Pos::new(40.0, 0.0, 500.0)));
    }

    #[test]
    fn a_walk_on_link_fires_once_and_arms_again_only_after_the_player_has_left_its_line() {
        let links = [link_at(0.0, 0.0, false)];
        let p = |x: f32| Pos::new(x, 0.0, 0.0);
        // a player that walks up to the line from afar fires it
        let mut arming = LinkArming::placed_at(&links, p(-200.0));
        assert!(arming.is_armed(0));
        assert_eq!(arming.step(&links, p(-200.0), p(-100.0)), None);
        assert_eq!(arming.step(&links, p(-100.0), p(-3.0)), Some(0));
        // placed on the other side of the area it is placed anew, with everything near it disarmed
        let mut arming = LinkArming::placed_at(&links, p(15.0));
        assert!(!arming.is_armed(0), "arrived 15 units from the line: not armed");
        assert_eq!(arming.step(&links, p(15.0), p(-5.0)), None, "walking back over the line does not fire it");
        assert!(!arming.is_armed(0), "and it is still not armed: the player is on it");
        // out of reach arms it again, and then it fires
        assert_eq!(arming.step(&links, p(-5.0), p(-35.0)), None);
        assert!(arming.is_armed(0));
        assert_eq!(arming.step(&links, p(-35.0), p(5.0)), Some(0));
        // standing ON the line and walking off it (the arrival of the stair of nmkin_4) fires nothing
        let mut arming = LinkArming::placed_at(&links, p(0.5));
        assert_eq!(arming.step(&links, p(0.5), p(-6.0)), None);
        assert_eq!(arming.step(&links, p(-6.0), p(-20.0)), None);
        assert!(!arming.is_armed(0), "20 units out is still within reach");
        assert_eq!(arming.step(&links, p(-20.0), p(-40.0)), None);
        assert!(arming.is_armed(0));
    }

    #[test]
    fn a_link_that_needs_ok_never_fires_by_walking_and_state_that_does_not_fit_is_made_anew() {
        let links = [link_at(0.0, 0.0, true), link_at(100.0, 0.0, false)];
        let p = |x: f32| Pos::new(x, 0.0, 0.0);
        let mut arming = LinkArming::placed_at(&links, p(-200.0));
        assert_eq!(arming.step(&links, p(-200.0), p(5.0)), None, "walking over an OK line is not pressing OK");
        assert_eq!(arming.step(&links, p(5.0), p(98.0)), Some(1));
        // a state made for another list of links is made again, from where the move started
        let mut stale = LinkArming::default();
        assert_eq!(stale.step(&links, p(-200.0), p(98.0)), Some(1));
        assert!(!stale.is_armed(7), "an index nobody knows is not armed");
    }

    #[test]
    fn a_press_point_is_pressed_in_its_region_and_a_talk_point_next_to_its_character_only() {
        let press = StoryPoint {
            id: 1,
            trigger: StoryTrigger::Press(vec![Pos::new(0.0, 0.0, 5.0), Pos::new(10.0, 0.0, 5.0), Pos::new(0.0, 10.0, 5.0)]),
            label: "panel".into(),
        };
        assert!(press.is_on_press() && !press.is_on_entry());
        assert!(press.is_pressed_at(Pos::new(2.0, 2.0, 5.0), None) && !press.is_pressed_at(Pos::new(9.0, 9.0, 5.0), None));
        assert!(!press.is_hit_by(Pos::new(-5.0, 2.0, 0.0), Pos::new(5.0, 2.0, 0.0)), "walking through it is not pressing");
        assert_eq!(press.center(), Pos::new(10.0 / 3.0, 10.0 / 3.0, 5.0));
        let talk = StoryPoint { id: 2, trigger: StoryTrigger::Talk { center: Pos::new(100.0, 100.0, 50.0), radius: 80.0 }, label: "talk".into() };
        assert!(talk.is_on_press());
        assert!(talk.is_pressed_at(Pos::new(100.0, 170.0, 50.0), None), "within its radius");
        assert!(!talk.is_pressed_at(Pos::new(100.0, 190.0, 50.0), None), "out of reach");
        assert!(!talk.is_pressed_at(Pos::new(100.0, 150.0, 50.0 + Link::LEVEL + 1.0), None), "another floor");
        assert!(talk.is_pressed_at(Pos::new(100.0, 150.0, 50.0), Some(Pos::new(101.0, 100.5, 50.0))), "the character the player addressed is the point's");
        assert!(!talk.is_pressed_at(Pos::new(100.0, 150.0, 50.0), Some(Pos::new(130.0, 100.0, 50.0))), "another character stands there");
        assert!(!talk.is_hit_by(Pos::new(0.0, 100.0, 50.0), Pos::new(100.0, 100.0, 50.0)), "walking up to a character is not talking to it");
        assert_eq!(talk.center(), Pos::new(100.0, 100.0, 50.0));
        // the other kinds are never pressed
        assert!(!region(&[(0.0, 0.0), (10.0, 0.0), (0.0, 10.0)]).is_pressed_at(Pos::new(2.0, 2.0, 0.0), None));
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
