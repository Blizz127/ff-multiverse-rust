//! Static analysis of the entities' init scripts (script 0 of every entity) and of the
//! `MAPJUMP` instructions in all scripts.
//!
//! **Script 0 is two scripts in one.** Its code runs from the entry point to the first `RET`
//! (the *init* part, executed once when the field loads) and the bytes after that `RET` are the
//! entity's *main* part (an idle loop such as `JMPB 0`, or logic that places and shows the
//! entity depending on story flags). The entry table has no slot for the main part, which is
//! why script 1 (the "talk" script of model entities) is a separate entry. This is how the
//! community tools present it (Makou Reactor lists "Init" and "Main" for script 0) and what the
//! owned data show: for example a character's script 0 is `CHAR; PC; RET` followed by
//! `IFSW ...; XYZI ...; DIR ...; VISI 0; JMPB 0`. The analysis therefore reads the whole of
//! script 0, with both parts as entry points, and labels every effect with the [`Phase`] it
//! belongs to.
//!
//! What the scripts declare: NPCs become field models with `CHAR`, are put on the walkmesh with
//! `XYZI`/`XYI`/`XYZ`, turned (`DIR`), shown or hidden (`VISI`), made solid or not (`SOLID`),
//! given talk and collision radii (`TALKR`, `SLIDR`), and the entities that stand for playable
//! characters are bound to a character id with `PC`.
//!
//! The analysis is purely static: it never evaluates variables or conditions. For each effect
//! it reports whether the instruction lies in a region only some paths execute (between a jump
//! and its target), so callers can decide what to do with story-dependent placements. An
//! operand that names a variable is reported as [`Arg::Variable`] rather than guessed.

use super::decode::{DecodeError, Disassembly, Instruction};
use super::layout::{INIT_SCRIPT, SCRIPTS_PER_ENTITY};
use super::models::{ModelList, ModelRef};
use super::FieldScript;
use crate::field::{Vertex, Walkmesh};
use std::collections::{BTreeMap, BTreeSet};

/// An operand that is either a literal, a variable reference, or not present in the opcode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arg<T> {
    Literal(T),
    /// Variable `address` of bank `bank` (not evaluated).
    Variable { bank: u8, address: u8 },
    /// The opcode has no such operand (`XYI` has no z, `XYZ` no triangle).
    Absent,
}

impl<T: Copy> Arg<T> {
    pub fn literal(&self) -> Option<T> {
        match self {
            Self::Literal(v) => Some(*v),
            _ => None,
        }
    }
}

/// The two parts of script 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Phase {
    /// From the entry point to the first `RET`: runs once when the field loads.
    Init,
    /// After the first `RET`: the entity's main part.
    Main,
}

/// A value taken from script 0 with the instruction that set it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tracked<T> {
    pub value: T,
    /// Section-relative offset of the instruction.
    pub offset: usize,
    pub phase: Phase,
    /// The instruction only runs on some paths (it lies between a jump and its target).
    pub conditional: bool,
    /// A later conditional instruction of the same kind may replace this value.
    pub may_be_overridden: bool,
}

/// Which opcode placed an entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlacementKind {
    /// `XYZI`: x, y, z and the walkmesh triangle.
    Xyzi,
    /// `XYI`: x, y and the triangle (z comes from the walkmesh).
    Xyi,
    /// `XYZ`: x, y, z (the triangle is looked up).
    Xyz,
}

/// One placement instruction of script 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    pub kind: PlacementKind,
    pub x: Arg<i16>,
    pub y: Arg<i16>,
    pub z: Arg<i16>,
    pub triangle: Arg<u16>,
    pub offset: usize,
    pub phase: Phase,
    pub conditional: bool,
}

impl Placement {
    /// `(x, y)` when both are literals.
    pub fn xy(&self) -> Option<(i16, i16)> {
        Some((self.x.literal()?, self.y.literal()?))
    }

    /// Whether every operand the opcode has is a literal.
    pub fn is_literal(&self) -> bool {
        let ok = |a: &Arg<i16>| !matches!(a, Arg::Variable { .. });
        ok(&self.x) && ok(&self.y) && ok(&self.z) && !matches!(self.triangle, Arg::Variable { .. })
    }
}

/// What script 0 of one entity declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityInit {
    pub entity: usize,
    pub name: String,
    /// Script 0 has code (its entry point lies in the script area).
    pub has_code: bool,
    /// Instructions of script 0 that are reachable from its two entry points.
    pub instructions: usize,
    /// Section offset of the first instruction of the main part (the one after the first `RET`),
    /// if the script has a main part.
    pub main_start: Option<usize>,
    /// Why decoding stopped early; `None` when the script decoded completely.
    pub decode_error: Option<DecodeError>,
    /// `CHAR`: the entity is a field model. This is the opcode's operand ("character id"),
    /// which does **not** select the model: the game hands out the model-loader records in
    /// order to the entities that run `CHAR` (see [`EntityInit::model_index`]).
    pub char_id: Option<Tracked<Arg<u8>>>,
    /// Index of this entity's record in the model loader: the number of entities before it
    /// (in entity order) whose script 0 has a `CHAR`. Set for every entity with a `CHAR`.
    pub model_index: Option<usize>,
    /// The model-loader record at `model_index` (when a model list was supplied).
    pub model_ref: Option<ModelRef>,
    /// `PC`: the entity stands for this playable character (character id 0..=8).
    pub player: Option<Tracked<Arg<u8>>>,
    /// Every placement instruction in program order.
    pub placements: Vec<Placement>,
    /// `DIR`: facing, 0..=255 (256 = full turn).
    pub direction: Option<Tracked<Arg<u8>>>,
    /// `VISI`: visibility flag (1 = visible, 0 = hidden).
    pub visible: Option<Tracked<Arg<u8>>>,
    /// `SOLID`: as stored; **0 = solid, 1 = not solid** (the page documents the switch as
    /// "on/off (0/1)").
    pub solid: Option<Tracked<Arg<u8>>>,
    /// `TALKR`: talk radius.
    pub talk_radius: Option<Tracked<Arg<u8>>>,
    /// `SLIDR`: collision radius.
    pub solid_radius: Option<Tracked<Arg<u8>>>,
}

impl EntityInit {
    /// The entity is a field model (has a `CHAR`).
    pub fn is_character(&self) -> bool {
        self.char_id.is_some()
    }

    /// The entity stands for a playable character (has a `PC`).
    pub fn is_player(&self) -> bool {
        self.player.is_some()
    }

    /// The placement that probably wins: the last unconditional one, else the last conditional
    /// one.
    pub fn placement(&self) -> Option<&Placement> {
        self.placements.iter().rev().find(|p| !p.conditional).or_else(|| self.placements.last())
    }

    /// Initial visibility when it is a literal.
    pub fn initially_visible(&self) -> Option<bool> {
        self.visible.and_then(|t| t.value.literal()).map(|v| v != 0)
    }

    /// Whether the entity starts solid, when `SOLID` is a literal (`SOLID 0` switches solidity on).
    pub fn initially_solid(&self) -> Option<bool> {
        self.solid.and_then(|t| t.value.literal()).map(|v| v == 0)
    }
}

/// Collects the last unconditional and last conditional value of one effect.
struct Slot<T> {
    unconditional: Option<(T, usize, Phase)>,
    conditional: Option<(T, usize, Phase)>,
}

impl<T: Copy> Slot<T> {
    fn new() -> Self {
        Self { unconditional: None, conditional: None }
    }

    fn set(&mut self, value: T, offset: usize, phase: Phase, conditional: bool) {
        if conditional {
            self.conditional = Some((value, offset, phase));
        } else {
            // an unconditional assignment supersedes everything before it
            self.unconditional = Some((value, offset, phase));
            self.conditional = None;
        }
    }

    fn finish(self) -> Option<Tracked<T>> {
        match (self.unconditional, self.conditional) {
            (Some((value, offset, phase)), later) => Some(Tracked { value, offset, phase, conditional: false, may_be_overridden: later.is_some() }),
            (None, Some((value, offset, phase))) => Some(Tracked { value, offset, phase, conditional: true, may_be_overridden: false }),
            (None, None) => None,
        }
    }
}

/// Reachable instruction offsets and the byte regions skipped by forward jumps.
struct Flow {
    reachable: BTreeSet<usize>,
    /// Disjoint, sorted `[from, to)` regions executed only on some paths.
    skipped: Vec<(usize, usize)>,
    /// Offset of the first `RET` in linear order (the end of the init part).
    first_ret: Option<usize>,
    /// Offset of the instruction after it (the start of the main part).
    main_start: Option<usize>,
}

impl Flow {
    /// Whether the instruction at `offset` lies in a region that only some paths execute.
    fn is_conditional(&self, offset: usize) -> bool {
        let after = self.skipped.partition_point(|&(from, _)| from <= offset);
        after.checked_sub(1).and_then(|i| self.skipped.get(i)).is_some_and(|&(_, to)| offset < to)
    }
}

/// Sort `[from, to)` regions and merge the overlapping or touching ones.
fn merge_regions(mut regions: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    regions.retain(|&(from, to)| from < to);
    regions.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::with_capacity(regions.len());
    for (from, to) in regions {
        match merged.last_mut() {
            Some(last) if from <= last.1 => last.1 = last.1.max(to),
            _ => merged.push((from, to)),
        }
    }
    merged
}

fn flow(d: &Disassembly) -> Flow {
    let index: BTreeMap<usize, usize> = d.instructions.iter().enumerate().map(|(i, ins)| (ins.offset, i)).collect();
    let first_ret_index = d.instructions.iter().position(|i| i.opcode == 0x00);
    let first_ret = first_ret_index.and_then(|i| d.instructions.get(i)).map(|i| i.offset);
    let main_start = first_ret_index.and_then(|i| d.instructions.get(i + 1)).map(|i| i.offset);

    let mut reachable = BTreeSet::new();
    let mut stack = Vec::new();
    if let Some(first) = d.instructions.first() {
        stack.push(first.offset);
    }
    if let Some(m) = main_start {
        stack.push(m);
    }
    while let Some(off) = stack.pop() {
        if !reachable.insert(off) {
            continue;
        }
        let Some(ins) = index.get(&off).and_then(|&i| d.instructions.get(i)) else { continue };
        if !ins.ends_flow() {
            let next = ins.end();
            if index.contains_key(&next) {
                stack.push(next);
            }
        }
        if let Some(t) = ins.jump_target {
            if index.contains_key(&t) {
                stack.push(t);
            }
        }
    }
    let skipped = merge_regions(
        d.instructions
            .iter()
            .filter(|ins| reachable.contains(&ins.offset))
            .filter_map(|ins| match ins.jump_target {
                Some(t) if ins.is_jump() && t > ins.offset => Some((ins.end(), t)),
                _ => None,
            })
            .collect(),
    );
    Flow { reachable, skipped, first_ret, main_start }
}

fn bank_ref(o: &super::decode::Operand) -> (u8, u8) {
    (o.bank, u8::try_from(o.value & 0xFF).unwrap_or(0))
}

fn arg_u8(ins: &Instruction, name: &str) -> Arg<u8> {
    match ins.operand(name) {
        None => Arg::Absent,
        Some(o) if o.bank == 0 => Arg::Literal(u8::try_from(o.value & 0xFF).unwrap_or(0)),
        Some(o) => {
            let (bank, address) = bank_ref(o);
            Arg::Variable { bank, address }
        }
    }
}

fn arg_i16(ins: &Instruction, name: &str) -> Arg<i16> {
    match ins.operand(name) {
        None => Arg::Absent,
        Some(o) if o.bank == 0 => Arg::Literal(i16::try_from(o.value).unwrap_or(0)),
        Some(o) => {
            let (bank, address) = bank_ref(o);
            Arg::Variable { bank, address }
        }
    }
}

fn arg_u16(ins: &Instruction, name: &str) -> Arg<u16> {
    match ins.operand(name) {
        None => Arg::Absent,
        Some(o) if o.bank == 0 => Arg::Literal(u16::try_from(o.value & 0xFFFF).unwrap_or(0)),
        Some(o) => {
            let (bank, address) = bank_ref(o);
            Arg::Variable { bank, address }
        }
    }
}

/// Summarise one entity's script 0 from its disassembly.
fn summarize(entity: usize, name: &str, d: Option<&Disassembly>) -> EntityInit {
    let mut out = EntityInit {
        entity,
        name: name.to_string(),
        has_code: d.is_some(),
        instructions: 0,
        main_start: None,
        decode_error: None,
        char_id: None,
        model_index: None,
        model_ref: None,
        player: None,
        placements: Vec::new(),
        direction: None,
        visible: None,
        solid: None,
        talk_radius: None,
        solid_radius: None,
    };
    let Some(d) = d else { return out };
    out.decode_error = d.error.clone();
    let flow = flow(d);
    out.main_start = flow.main_start;
    let is_conditional = |off: usize| flow.is_conditional(off);
    let phase_of = |off: usize| match flow.first_ret {
        Some(r) if off > r => Phase::Main,
        _ => Phase::Init,
    };

    let mut model = Slot::new();
    let mut player = Slot::new();
    let mut direction = Slot::new();
    let mut visible = Slot::new();
    let mut solid = Slot::new();
    let mut talk = Slot::new();
    let mut slide = Slot::new();
    for ins in d.instructions.iter().filter(|i| flow.reachable.contains(&i.offset)) {
        out.instructions += 1;
        let (cond, phase) = (is_conditional(ins.offset), phase_of(ins.offset));
        match ins.opcode {
            0xA1 => model.set(arg_u8(ins, "char_id"), ins.offset, phase, cond),
            0xA0 => player.set(arg_u8(ins, "character"), ins.offset, phase, cond),
            0xA5..=0xA7 => {
                let kind = match ins.opcode {
                    0xA5 => PlacementKind::Xyzi,
                    0xA6 => PlacementKind::Xyi,
                    _ => PlacementKind::Xyz,
                };
                out.placements.push(Placement {
                    kind,
                    x: arg_i16(ins, "x"),
                    y: arg_i16(ins, "y"),
                    z: arg_i16(ins, "z"),
                    triangle: arg_u16(ins, "triangle"),
                    offset: ins.offset,
                    phase,
                    conditional: cond,
                });
            }
            0xB3 => direction.set(arg_u8(ins, "direction"), ins.offset, phase, cond),
            0xA4 => visible.set(arg_u8(ins, "visible"), ins.offset, phase, cond),
            0xC7 => solid.set(arg_u8(ins, "solid"), ins.offset, phase, cond),
            0xC5 => talk.set(arg_u8(ins, "radius"), ins.offset, phase, cond),
            0xC6 => slide.set(arg_u8(ins, "radius"), ins.offset, phase, cond),
            _ => {}
        }
    }
    out.char_id = model.finish();
    out.player = player.finish();
    out.direction = direction.finish();
    out.visible = visible.finish();
    out.solid = solid.finish();
    out.talk_radius = talk.finish();
    out.solid_radius = slide.finish();
    out
}

/// A `MAPJUMP` instruction found in a script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapJump {
    pub entity: usize,
    pub entity_name: String,
    /// The first slot (0..32) that points at the script holding the instruction (several slots
    /// can share one script).
    pub script: usize,
    /// Section-relative offset of the instruction.
    pub offset: usize,
    /// Field id (index into the maplist); `Arg::Variable` is impossible for this opcode.
    pub field_id: u16,
    pub x: i16,
    pub y: i16,
    /// The fourth operand: the destination walkmesh triangle (the page calls it "Z").
    pub triangle: i16,
    pub direction: u8,
    /// The instruction lies in script 0's region that only some paths execute.
    pub conditional: bool,
}

impl FieldScript<'_> {
    /// Analyse script 0 of every entity, in entity order. Pass the field's parsed model list
    /// (see [`super::parse_model_list`]) to resolve model indices to names.
    pub fn entity_inits(&self, models: Option<&ModelList>) -> Vec<EntityInit> {
        let mut inits: Vec<EntityInit> = (0..self.entity_count())
            .map(|entity| {
                let name = self.entity_name(entity).unwrap_or("");
                let d = self.disassemble_partial(entity, INIT_SCRIPT).ok();
                summarize(entity, name, d.as_ref())
            })
            .collect();
        // The game gives the model-loader records, in order, to the entities that run CHAR.
        for (index, init) in inits.iter_mut().filter(|i| i.char_id.is_some()).enumerate() {
            init.model_index = Some(index);
            init.model_ref = models.and_then(|m| m.models.get(index)).cloned();
        }
        inits
    }

    /// Entities whose script 0 declares them as field models (has a `CHAR`).
    pub fn character_entities(&self, models: Option<&ModelList>) -> Vec<EntityInit> {
        self.entity_inits(models).into_iter().filter(EntityInit::is_character).collect()
    }

    /// `(entity, character id)` for every entity bound to a playable character with a literal
    /// `PC` operand, in entity order. A field has one such entity per character that may appear
    /// in it (the party decides at run time which of them is shown and controlled).
    pub fn player_entities(&self) -> Vec<(usize, u8)> {
        self.entity_inits(None)
            .into_iter()
            .filter_map(|e| Some((e.entity, e.player?.value.literal()?)))
            .collect()
    }

    /// The entity standing for playable character `character` (0 = the first character).
    pub fn player_entity(&self, character: u8) -> Option<usize> {
        self.player_entities().into_iter().find(|&(_, c)| c == character).map(|(e, _)| e)
    }

    /// Every `MAPJUMP` with literal operands in the field's scripts, one entry per distinct
    /// script (slots that share an entry point are reported once, under the first slot).
    pub fn map_jumps(&self) -> Vec<MapJump> {
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for entity in 0..self.entity_count() {
            for slot in 0..SCRIPTS_PER_ENTITY {
                let Some(span) = self.span(entity, slot) else { continue };
                if !seen.insert(span.start) {
                    continue;
                }
                let Ok(d) = self.disassemble_partial(entity, slot) else { continue };
                let script0_flow = if slot == INIT_SCRIPT { Some(flow(&d)) } else { None };
                for ins in d.instructions.iter().filter(|i| i.opcode == 0x60) {
                    let (Some(field), Some(x), Some(y), Some(triangle), Some(direction)) =
                        (ins.literal("field"), ins.literal("x"), ins.literal("y"), ins.literal("triangle"), ins.literal("direction"))
                    else {
                        continue;
                    };
                    let conditional = script0_flow.as_ref().is_some_and(|f| f.is_conditional(ins.offset));
                    out.push(MapJump {
                        entity,
                        entity_name: self.entity_name(entity).unwrap_or("").to_string(),
                        script: slot,
                        offset: ins.offset,
                        field_id: u16::try_from(field).unwrap_or(0),
                        x: i16::try_from(x).unwrap_or(0),
                        y: i16::try_from(y).unwrap_or(0),
                        triangle: i16::try_from(triangle).unwrap_or(0),
                        direction: u8::try_from(direction).unwrap_or(0),
                        conditional,
                    });
                }
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------------------
// Placements against the walkmesh
// ---------------------------------------------------------------------------------------

/// How a placement relates to the walkmesh.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WalkmeshFit {
    /// `(x, y)` lies inside (or on the border of) the named triangle. `height_error` is
    /// `z - interpolated triangle height` when the placement has a literal z.
    Inside { height_error: Option<f64> },
    /// `(x, y)` lies outside the named triangle by `distance` walkmesh units; `containing` is
    /// another triangle that does contain it, if any.
    Outside { distance: f64, containing: Option<usize> },
    /// The placement names a triangle index past the end of the mesh.
    NoSuchTriangle,
    /// No triangle was named (`XYZ`); the point lies in triangle `found` (or none).
    Unnamed { found: Option<usize> },
}

fn cross(ax: i64, ay: i64, bx: i64, by: i64, px: i64, py: i64) -> i64 {
    (bx - ax) * (py - ay) - (by - ay) * (px - ax)
}

/// Whether `(x, y)` lies inside triangle `tri` in the xy-plane (borders included, either
/// winding).
pub fn triangle_contains_xy(tri: &[Vertex; 3], x: i32, y: i32) -> bool {
    let [a, b, c] = tri;
    // a degenerate (zero-area) triangle contains nothing
    if cross(i64::from(a.x), i64::from(a.y), i64::from(b.x), i64::from(b.y), i64::from(c.x), i64::from(c.y)) == 0 {
        return false;
    }
    let (px, py) = (i64::from(x), i64::from(y));
    let d1 = cross(i64::from(a.x), i64::from(a.y), i64::from(b.x), i64::from(b.y), px, py);
    let d2 = cross(i64::from(b.x), i64::from(b.y), i64::from(c.x), i64::from(c.y), px, py);
    let d3 = cross(i64::from(c.x), i64::from(c.y), i64::from(a.x), i64::from(a.y), px, py);
    let neg = d1 < 0 || d2 < 0 || d3 < 0;
    let pos = d1 > 0 || d2 > 0 || d3 > 0;
    !(neg && pos)
}

/// Distance in the xy-plane from `(x, y)` to the nearest point of triangle `tri` (0 inside).
pub fn distance_to_triangle_xy(tri: &[Vertex; 3], x: i32, y: i32) -> f64 {
    if triangle_contains_xy(tri, x, y) {
        return 0.0;
    }
    let [a, b, c] = tri;
    let p = (f64::from(x), f64::from(y));
    let seg = |u: &Vertex, v: &Vertex| -> f64 {
        let (ux, uy, vx, vy) = (f64::from(u.x), f64::from(u.y), f64::from(v.x), f64::from(v.y));
        let (dx, dy) = (vx - ux, vy - uy);
        let len2 = dx * dx + dy * dy;
        let t = if len2 == 0.0 { 0.0 } else { (((p.0 - ux) * dx + (p.1 - uy) * dy) / len2).clamp(0.0, 1.0) };
        let (qx, qy) = (ux + t * dx, uy + t * dy);
        ((p.0 - qx).powi(2) + (p.1 - qy).powi(2)).sqrt()
    };
    seg(a, b).min(seg(b, c)).min(seg(c, a))
}

/// Height of triangle `tri` above `(x, y)` by barycentric interpolation of the vertex z values
/// (`None` for a degenerate triangle).
pub fn triangle_height_at(tri: &[Vertex; 3], x: i32, y: i32) -> Option<f64> {
    let [a, b, c] = tri;
    let (ax, ay, bx, by, cx, cy) = (f64::from(a.x), f64::from(a.y), f64::from(b.x), f64::from(b.y), f64::from(c.x), f64::from(c.y));
    let det = (by - cy) * (ax - cx) + (cx - bx) * (ay - cy);
    if det == 0.0 {
        return None;
    }
    let (px, py) = (f64::from(x), f64::from(y));
    let l1 = ((by - cy) * (px - cx) + (cx - bx) * (py - cy)) / det;
    let l2 = ((cy - ay) * (px - cx) + (ax - cx) * (py - cy)) / det;
    let l3 = 1.0 - l1 - l2;
    Some(l1 * f64::from(a.z) + l2 * f64::from(b.z) + l3 * f64::from(c.z))
}

/// First triangle of `mesh` that contains `(x, y)`.
pub fn locate_on_walkmesh(mesh: &Walkmesh, x: i32, y: i32) -> Option<usize> {
    mesh.triangles.iter().position(|t| triangle_contains_xy(t, x, y))
}

/// Check a placement against the walkmesh. `None` when `x` or `y` is not a literal.
pub fn check_placement(mesh: &Walkmesh, p: &Placement) -> Option<WalkmeshFit> {
    let (x, y) = p.xy()?;
    let (x, y) = (i32::from(x), i32::from(y));
    match p.triangle {
        Arg::Literal(index) => {
            let Some(tri) = mesh.triangles.get(usize::from(index)) else { return Some(WalkmeshFit::NoSuchTriangle) };
            if triangle_contains_xy(tri, x, y) {
                let height_error = p.z.literal().and_then(|z| triangle_height_at(tri, x, y).map(|h| f64::from(z) - h));
                Some(WalkmeshFit::Inside { height_error })
            } else {
                Some(WalkmeshFit::Outside { distance: distance_to_triangle_xy(tri, x, y), containing: locate_on_walkmesh(mesh, x, y) })
            }
        }
        Arg::Absent => Some(WalkmeshFit::Unnamed { found: locate_on_walkmesh(mesh, x, y) }),
        Arg::Variable { .. } => None,
    }
}
