//! FF7 story: per-account progression, the phases it selects, and the points that advance it.
//!
//! # The model
//!
//! FF7 keeps its main story position in one field variable, a 16-bit word at **bank 2, address 0**
//! (`Var[2][0]`; the community calls it the "game moment" or main progress counter). The field
//! scripts compare it everywhere (391 of the 702 owned fields read it, see
//! `examples/story_survey.rs`) and 213 write it. This module turns that into the three things the
//! multiplayer server needs, all per ACCOUNT and independent of which character a player looks like
//! or fights as:
//!
//! * **The progression blob** ([`StoryState`]): `[1, moment_lo, moment_hi]`, three bytes. The
//!   **empty blob is a new account** and means [`NEW_GAME_MOMENT`] (1): the value the opening scene
//!   of `md1stin` leaves behind (`SETWORD Var[2][0] = 1`, then control is handed to the player), so
//!   a player starts where the game first lets them walk. Every account that exists today has no
//!   progression record and is therefore in the same state. Anything else that is not exactly
//!   version 1 is unreadable and is never interpreted, advanced or overwritten.
//! * **The phase of an area** ([`AreaStory::phase_key`]): each field is partitioned by the moments
//!   its own scripts compare `Var[2][0]` against (`==`, `!=`, `<`, `>`, `<=`, `>=` with a literal,
//!   in any script of the field). Two moments in one interval make every one of those comparisons
//!   answer the same, so the field looks the same to both players; the key of an interval is its
//!   first moment. **The interval that holds [`NEW_GAME_MOMENT`] is the empty key in every field**,
//!   which is what makes all existing accounts one phase and keeps their placements byte for byte
//!   what they were.
//! * **Story points** ([`AreaStory::points`]): where a field's own scripts advance the moment on
//!   something the server can see. See below.
//!
//! # The one kind of story point: step onto a walkmesh triangle
//!
//! FF7 fields advance the story from small loops in an entity's script 0 (its "main" part, after
//! the first `RET`). The first fields of the first mission do it like this (offsets are in the
//! owned Steam copy; `md1stin`'s director entity, `md1_1`'s the same shape with `<` for `==`):
//!
//! ```text
//! init:   SETWORD Var[6][9] = 12              ; the triangle that matters
//!         RET
//! main:   GETAI entity=1 Var[6][4]            ; which triangle is the (controlled) player on
//!         IFSW Var[6][4] == Var[6][9]         ; ...is it triangle 12?
//!         IFSW Var[2][0] == 1                 ; ...and has the story not moved on
//!         SETWORD Var[2][0] = 6               ; story advances (then a cutscene and BATTLE 300)
//!         JMPB main
//! ```
//!
//! The server cannot run scripts, but it can see this: it knows where a player stands and which
//! triangle that is. [`AreaStory::scan`] therefore recognises exactly this pattern, and **only
//! this**, with a witness check on the loaded script and on the loaded walkmesh:
//!
//! * a `SETWORD`/`SETBYTE` of `Var[2][0]` with a literal, in the main part of an entity's script 0;
//! * every conditional jump that can skip it is one of: a comparison of `Var[2][0]` with a literal
//!   (any of the six ordered comparisons, at least one), or one equality between a variable that a
//!   `GETAI` in the same loop fills with the triangle of entity E and a variable that the init part
//!   set to a literal (exactly one such test, because it is the region);
//! * E is **bound to a playable character** (has a `PC`): any of them, never one in particular, so
//!   the rule is "the player's character", not "Cloud". The same variable is then the position of
//!   every player who stands for that role;
//! * the triangle exists in the walkmesh.
//!
//! A pattern that does not match every point (another edition, a field that does it differently, a
//! write that is not guarded by a region) is not a story point: **the server never invents one**.
//! The script's `BATTLE` after the write is not run (the server has no scripted battles yet); it is
//! named in the point's label so a log says so. Nothing else of the cutscene happens either: the
//! story simply moves.
//!
//! Advancing is monotone: a point sets the moment only if the guards hold for the player's current
//! moment **and** the new value is higher.
//!
//! # What this does not do
//!
//! * No cast per phase for FF7 yet: [`AreaRuntime::npcs_in_phase`] keeps the server's existing
//!   behaviour (the NPCs the data places unconditionally), so every account sees exactly the NPCs it
//!   always saw. The phase key is what a later lane keys the canon cast on.
//! * Only `Var[2][0]` is modelled. Other story variables (the bits NPCs and items hang off) are not
//!   part of the progression yet; the blob is versioned so they can be added.
//!
//! # Untrusted data
//!
//! Like the rest of the crate: no indexing, no unchecked arithmetic, no panics on script bytes.

use std::collections::{BTreeMap, BTreeSet};

use fmv_adapter_api::{StoryPoint, StoryStep, StoryTrigger};
use fmv_core::{PhaseKey, Pos};

use crate::field::Walkmesh;
use crate::script::{FieldScript, Instruction, Operand, INIT_SCRIPT, SCRIPTS_PER_ENTITY};

/// The bank of the main progress counter (`Var[2][0]`).
pub const MOMENT_BANK: u8 = 2;
/// The address of the main progress counter within its bank.
pub const MOMENT_ADDRESS: u8 = 0;
/// What a new account's (empty) progression stands for: the value `md1stin`'s opening scene leaves
/// the counter at before it gives the player control.
pub const NEW_GAME_MOMENT: u16 = 1;
/// The format version byte of a non-empty progression blob.
pub const BLOB_VERSION: u8 = 1;
/// First byte of every non-default [`PhaseKey`] this module makes.
const PHASE_TAG: u8 = 0x4D;
/// Most story points one field may offer (a real field has at most a few).
pub const MAX_STORY_POINTS: usize = 16;

/// An account's FF7 story state: where its main story stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoryState {
    /// The main progress counter (`Var[2][0]`).
    pub moment: u16,
}

impl StoryState {
    /// Where a new account starts ([`NEW_GAME_MOMENT`]).
    pub const fn new_game() -> Self {
        Self { moment: NEW_GAME_MOMENT }
    }

    /// Read a progression blob: empty is a new account, `[1, lo, hi]` is a stored moment, anything
    /// else is unreadable (`None`).
    pub fn decode(blob: &[u8]) -> Option<Self> {
        match blob {
            [] => Some(Self::new_game()),
            [BLOB_VERSION, lo, hi] => Some(Self { moment: u16::from_le_bytes([*lo, *hi]) }),
            _ => None,
        }
    }

    /// The blob that stores this state.
    pub fn encode(self) -> Vec<u8> {
        let [lo, hi] = self.moment.to_le_bytes();
        vec![BLOB_VERSION, lo, hi]
    }
}

/// The progression to store when `advanced` is saved over `stored`: whichever records the further
/// moment, so that a story never goes back and a late save cannot undo a later one. A `stored` blob
/// that cannot be read stays as it is (it is never overwritten), and so does one when `advanced`
/// cannot be read.
pub fn merge_progression(stored: &[u8], advanced: &[u8]) -> Vec<u8> {
    match (StoryState::decode(stored), StoryState::decode(advanced)) {
        (Some(s), Some(a)) if a.moment > s.moment => advanced.to_vec(),
        _ => stored.to_vec(),
    }
}

/// How a script compares the counter with a literal (the codes of `IFUB`/`IFSW`/`IFUW`'s `op`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cmp {
    Eq,
    Ne,
    Gt,
    Lt,
    Ge,
    Le,
}

impl Cmp {
    /// The ordered comparisons: 0 `==`, 1 `!=`, 2 `>`, 3 `<`, 4 `>=`, 5 `<=`. The other codes are bit
    /// tests (not comparisons of a value) and are not understood.
    fn from_code(code: i32) -> Option<Self> {
        Some(match code {
            0 => Self::Eq,
            1 => Self::Ne,
            2 => Self::Gt,
            3 => Self::Lt,
            4 => Self::Ge,
            5 => Self::Le,
            _ => return None,
        })
    }

    /// `moment <self> literal`.
    fn holds(self, moment: u16, literal: i32) -> bool {
        let m = i32::from(moment);
        match self {
            Self::Eq => m == literal,
            Self::Ne => m != literal,
            Self::Gt => m > literal,
            Self::Lt => m < literal,
            Self::Ge => m >= literal,
            Self::Le => m <= literal,
        }
    }

    /// `literal <self> moment` read from the counter's side.
    fn mirrored(self) -> Self {
        match self {
            Self::Gt => Self::Lt,
            Self::Lt => Self::Gt,
            Self::Ge => Self::Le,
            Self::Le => Self::Ge,
            same => same,
        }
    }

    fn symbol(self) -> &'static str {
        match self {
            Self::Eq => "==",
            Self::Ne => "!=",
            Self::Gt => ">",
            Self::Lt => "<",
            Self::Ge => ">=",
            Self::Le => "<=",
        }
    }

    /// The moments at which this comparison with `literal` starts answering differently from the
    /// moment before: the edges of the intervals it cuts the axis into.
    fn edges(self, literal: i32) -> [Option<i32>; 2] {
        let next = literal.saturating_add(1);
        match self {
            Self::Eq | Self::Ne => [Some(literal), Some(next)],
            Self::Gt | Self::Le => [Some(next), None],
            Self::Lt | Self::Ge => [Some(literal), None],
        }
    }
}

/// The opcodes that compare two values and jump when the answer is false.
const COMPARES: [&str; 6] = ["IFUB", "IFUBL", "IFSW", "IFSWL", "IFUW", "IFUWL"];

fn is_moment(o: &Operand) -> bool {
    o.bank == MOMENT_BANK && (o.value & 0xFF) == i32::from(MOMENT_ADDRESS)
}

/// `(bank, address)` of a banked operand.
fn var_of(o: &Operand) -> (u8, u8) {
    (o.bank, u8::try_from(o.value & 0xFF).unwrap_or(0))
}

/// `left <op> right` of a comparison instruction, when `op` is an ordered comparison.
fn compare_parts(ins: &Instruction) -> Option<(&Operand, &Operand, Cmp)> {
    if !COMPARES.contains(&ins.mnemonic) {
        return None;
    }
    let op = Cmp::from_code(ins.operand("op")?.value)?;
    Some((ins.operand("left")?, ins.operand("right")?, op))
}

/// `moment <cmp> literal` when the instruction compares the counter with a literal (either way
/// round).
fn moment_compare(ins: &Instruction) -> Option<(Cmp, i32)> {
    let (left, right, op) = compare_parts(ins)?;
    if is_moment(left) && right.bank == 0 {
        Some((op, right.value))
    } else if is_moment(right) && left.bank == 0 {
        Some((op.mirrored(), left.value))
    } else {
        None
    }
}

/// `(variable, literal)` of a `SETBYTE`/`SETWORD` of a variable with a literal.
fn literal_set(ins: &Instruction) -> Option<((u8, u8), i32)> {
    if !matches!(ins.mnemonic, "SETBYTE" | "SETWORD") {
        return None;
    }
    let (var, value) = (ins.operand("var")?, ins.operand("value")?);
    (var.bank != 0 && value.bank == 0).then(|| (var_of(var), value.value))
}

/// The jumps before `list[k]` that skip over it when taken: the forward jumps whose target lies
/// beyond it. Instruction `k` runs only if every one of them did NOT jump.
fn guards_of(list: &[Instruction], k: usize) -> Vec<&Instruction> {
    let Some(at) = list.get(k).map(|i| i.offset) else { return Vec::new() };
    list.get(..k)
        .unwrap_or(&[])
        .iter()
        .filter(|g| g.is_jump() && g.jump_target.is_some_and(|t| t > at) && g.end() <= at)
        .collect()
}

/// One story point as the script defines it (see the module docs).
#[derive(Debug, Clone, PartialEq)]
struct PointDef {
    /// The walkmesh triangle that is the region.
    triangle: usize,
    corners: [Pos; 3],
    /// The comparisons of the counter that must all hold.
    requires: Vec<(Cmp, i32)>,
    /// What the counter is set to.
    sets: u16,
    /// A `BATTLE` the script starts right after (not run by the server).
    battle: Option<u16>,
}

impl PointDef {
    fn applies_to(&self, moment: u16) -> bool {
        self.sets > moment && self.requires.iter().all(|&(cmp, literal)| cmp.holds(moment, literal))
    }

    fn label(&self) -> String {
        let when: Vec<String> = self.requires.iter().map(|&(cmp, literal)| format!("{}{literal}", cmp.symbol())).collect();
        let battle = self.battle.map_or(String::new(), |b| format!(" (the script then starts battle {b}, which the server does not run)"));
        format!("triangle {}: moment {} -> {}{battle}", self.triangle, when.join(" and "), self.sets)
    }
}

/// What one field's scripts say about the story: the intervals of the counter that make its phases,
/// and its story points.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AreaStory {
    /// First moment of every interval after the first, ascending (all in `1..=65535`).
    edges: Vec<u16>,
    points: Vec<PointDef>,
}

impl AreaStory {
    /// Read a field's story out of its script and walkmesh (see the module docs). A script that
    /// cannot be decoded in places contributes what did decode.
    pub fn scan(script: &FieldScript, mesh: &Walkmesh) -> Self {
        let mut edges = BTreeSet::new();
        let mut seen = BTreeSet::new();
        for entity in 0..script.entity_count() {
            for slot in 0..SCRIPTS_PER_ENTITY {
                // Slots that share an entry point are one script.
                let Some(span) = script.span(entity, slot) else { continue };
                if !seen.insert(span.start) {
                    continue;
                }
                let Ok(listing) = script.disassemble_partial(entity, slot) else { continue };
                for (cmp, literal) in listing.instructions.iter().filter_map(moment_compare) {
                    edges.extend(cmp.edges(literal).into_iter().flatten().filter_map(|e| u16::try_from(e).ok()).filter(|&e| e >= 1));
                }
            }
        }
        let players: BTreeSet<usize> = script.player_entities().into_iter().map(|(entity, _)| entity).collect();
        let mut points = Vec::new();
        for entity in 0..script.entity_count() {
            entity_points(script, entity, &players, mesh, &mut points);
        }
        points.truncate(MAX_STORY_POINTS);
        Self { edges: edges.into_iter().collect(), points }
    }

    /// The first moment of the interval that holds `moment`.
    fn interval_start(&self, moment: u16) -> u16 {
        self.edges.iter().rev().find(|&&e| e <= moment).copied().unwrap_or(0)
    }

    /// The phase of this field for a progression blob: the empty key for the interval that holds
    /// [`NEW_GAME_MOMENT`], else a tagged key naming the interval. `None` for a blob that cannot be
    /// read.
    pub fn phase_key(&self, blob: &[u8]) -> Option<PhaseKey> {
        let start = self.interval_start(StoryState::decode(blob)?.moment);
        if start == self.interval_start(NEW_GAME_MOMENT) {
            return Some(PhaseKey::default());
        }
        let [lo, hi] = start.to_le_bytes();
        Some(PhaseKey(vec![PHASE_TAG, lo, hi]))
    }

    /// The story points, `id` being the index.
    pub fn points(&self) -> Vec<StoryPoint> {
        self.points
            .iter()
            .enumerate()
            .map(|(i, p)| StoryPoint {
                id: u32::try_from(i).unwrap_or(u32::MAX),
                trigger: StoryTrigger::Region(p.corners.to_vec()),
                label: p.label(),
            })
            .collect()
    }

    /// What reaching point `id` does for `blob` (see `AreaRuntime::story_advance`).
    pub fn advance(&self, blob: &[u8], id: u32) -> Option<StoryStep> {
        let state = StoryState::decode(blob)?;
        let point = self.points.get(usize::try_from(id).ok()?)?;
        point.applies_to(state.moment).then(|| StoryStep {
            progress: StoryState { moment: point.sets }.encode(),
            summary: format!("moment {} -> {}", state.moment, point.sets),
        })
    }

    /// The first moment of every interval after the first (for tests and surveys).
    pub fn edges(&self) -> &[u16] {
        &self.edges
    }

    /// How many story points the field has.
    pub fn point_count(&self) -> usize {
        self.points.len()
    }
}

/// The story points of one entity's script 0 (see the module docs for the pattern).
fn entity_points(script: &FieldScript, entity: usize, players: &BTreeSet<usize>, mesh: &Walkmesh, out: &mut Vec<PointDef>) {
    let Ok(listing) = script.disassemble_partial(entity, INIT_SCRIPT) else { return };
    let code = listing.instructions.as_slice();
    let Some(first_ret) = code.iter().position(|i| i.opcode == 0x00) else { return };
    let init = code.get(..first_ret).unwrap_or(&[]);
    let main = code.get(first_ret + 1..).unwrap_or(&[]);

    // What the init part sets to a literal on every path.
    let mut constants: BTreeMap<(u8, u8), i32> = BTreeMap::new();
    for (k, ins) in init.iter().enumerate() {
        if let Some((var, value)) = literal_set(ins) {
            if guards_of(init, k).is_empty() {
                constants.insert(var, value);
            }
        }
    }

    // Variables that hold the triangle of an entity, as the loop reads them.
    let mut triangle_vars: BTreeMap<(u8, u8), usize> = BTreeMap::new();
    for (k, ins) in main.iter().enumerate() {
        if ins.mnemonic == "GETAI" {
            if let (Some(target), Some(var)) = (ins.literal("entity"), ins.operand("var_triangle")) {
                if let (Ok(target), true) = (usize::try_from(target), var.bank != 0) {
                    triangle_vars.insert(var_of(var), target);
                }
            }
            continue;
        }
        let Some((var, value)) = literal_set(ins) else { continue };
        if var != (MOMENT_BANK, MOMENT_ADDRESS) {
            continue;
        }
        if let Some(point) = point_at(main, k, value, &constants, &triangle_vars, players, mesh) {
            out.push(point);
        }
    }
}

/// The story point that the `SETWORD`/`SETBYTE` `main[k]` of the counter (to `value`) makes, if
/// every jump that can skip it is understood and it is a step onto a triangle.
fn point_at(
    main: &[Instruction],
    k: usize,
    value: i32,
    constants: &BTreeMap<(u8, u8), i32>,
    triangle_vars: &BTreeMap<(u8, u8), usize>,
    players: &BTreeSet<usize>,
    mesh: &Walkmesh,
) -> Option<PointDef> {
    let sets = u16::try_from(value).ok()?;
    let guards = guards_of(main, k);
    let mut triangle: Option<(usize, usize)> = None;
    let mut requires = Vec::new();
    for guard in &guards {
        if let Some(compare) = moment_compare(guard) {
            requires.push(compare);
        } else {
            // A jump the server cannot judge makes the whole write no point.
            let found = triangle_guard(guard, constants, triangle_vars)?;
            if triangle.replace(found).is_some() {
                return None; // two region tests: not the pattern
            }
        }
    }
    let (triangle, entity) = triangle?;
    if requires.is_empty() || !players.contains(&entity) {
        return None;
    }
    let verts = mesh.triangles.get(triangle)?;
    let corner = |i: usize| verts.get(i).map(|v| Pos::new(f32::from(v.x), f32::from(v.y), f32::from(v.z)));
    let corners = [corner(0)?, corner(1)?, corner(2)?];
    // A battle the script starts inside the same guarded block, before the nearest guard's target.
    let block_end = guards.iter().filter_map(|g| g.jump_target).min();
    let battle = main
        .iter()
        .skip(k + 1)
        .take_while(|i| block_end.is_none_or(|end| i.offset < end))
        .find(|i| i.mnemonic == "BATTLE")
        .and_then(|i| i.literal("battle"))
        .and_then(|b| u16::try_from(b).ok());
    Some(PointDef { triangle, corners, requires, sets, battle })
}

/// `(triangle, entity)` when `guard` tests that a variable filled with the triangle of `entity` equals
/// a variable the init part set to a literal.
fn triangle_guard(
    guard: &Instruction,
    constants: &BTreeMap<(u8, u8), i32>,
    triangle_vars: &BTreeMap<(u8, u8), usize>,
) -> Option<(usize, usize)> {
    let (left, right, op) = compare_parts(guard)?;
    if op != Cmp::Eq || left.bank == 0 || right.bank == 0 {
        return None;
    }
    let pick = |tri: (u8, u8), constant: (u8, u8)| Some((usize::try_from(*constants.get(&constant)?).ok()?, *triangle_vars.get(&tri)?));
    let (l, r) = (var_of(left), var_of(right));
    pick(l, r).or_else(|| pick(r, l))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::field::Vertex;
    use crate::script::synth::SynthScript;

    // ---- a tiny assembler for the opcodes the story reads ----

    fn ret() -> Vec<u8> {
        vec![0x00]
    }
    /// `SETWORD Var[bank][addr] = value`.
    fn setword(bank: u8, addr: u8, value: i16) -> Vec<u8> {
        let v = value.to_le_bytes();
        vec![0x81, bank << 4, addr, v[0], v[1]]
    }
    /// `GETAI entity -> Var[bank][addr]`.
    fn getai(entity: u8, bank: u8, addr: u8) -> Vec<u8> {
        vec![0xB9, bank, entity, addr]
    }
    /// `IFSW Var[lb][la] <op> Var[rb][ra]` (a variable on each side); `jump` is counted from the jump
    /// operand (the last byte) and taken when the comparison is false.
    fn ifsw_vars(lb: u8, la: u8, rb: u8, ra: u8, op: u8, jump: u8) -> Vec<u8> {
        vec![0x16, (lb << 4) | rb, la, 0, ra, 0, op, jump]
    }
    /// `IFSW Var[bank][addr] <op> literal`.
    fn ifsw_lit(bank: u8, addr: u8, literal: i16, op: u8, jump: u8) -> Vec<u8> {
        let l = literal.to_le_bytes();
        vec![0x16, bank << 4, addr, 0, l[0], l[1], op, jump]
    }
    fn concat(parts: &[Vec<u8>]) -> Vec<u8> {
        parts.concat()
    }

    /// `n` triangles side by side (triangle `i` at x = 100 i).
    fn mesh(n: usize) -> Walkmesh {
        let tri = |i: i16| {
            let o = i * 100;
            [Vertex { x: o, y: 0, z: 5 }, Vertex { x: o + 90, y: 0, z: 5 }, Vertex { x: o, y: 90, z: 5 }]
        };
        let count = i16::try_from(n).unwrap();
        Walkmesh { triangles: (0..count).map(tri).collect(), access: vec![[0xFFFF; 3]; n] }
    }

    /// The first mission's pattern: `dir` runs the loop, `hero` is the entity it asks about and is
    /// bound to the playable character `pc` (`None`: bound to none).
    fn field_script(set_to: i16, moment_op: u8, moment_lit: i16, pc: Option<u8>) -> Vec<u8> {
        // The loop: GETAI, two IFSW that skip to the JMPB at its end, the write, JMPB back to the top.
        let tail = concat(&[setword(2, 0, set_to), vec![0xBF, 0x01]]); // SETWORD, then CC (any instruction)
        let second = ifsw_lit(2, 0, moment_lit, moment_op, u8::try_from(tail.len() + 1).unwrap());
        let first = ifsw_vars(6, 4, 6, 9, 0, u8::try_from(second.len() + tail.len() + 1).unwrap());
        let top = getai(1, 6, 4);
        let jmpb = vec![0x12, u8::try_from(top.len() + first.len() + second.len() + tail.len()).unwrap()];
        let init = concat(&[setword(6, 9, 12), ret()]);
        let director = concat(&[init, top, first, second, tail, jmpb]);
        let hero = match pc {
            Some(c) => concat(&[vec![0xA1, 0x00], vec![0xA0, c], ret()]),
            None => concat(&[vec![0xA1, 0x00], ret()]),
        };
        SynthScript { model_count: 1, scale: 512, ..Default::default() }.entity("dir", vec![(0, director)]).entity("hero", vec![(0, hero)]).build()
    }

    fn scan(section: &[u8], mesh: &Walkmesh) -> AreaStory {
        AreaStory::scan(&FieldScript::parse(section).unwrap(), mesh)
    }

    fn at(moment: u16) -> Vec<u8> {
        StoryState { moment }.encode()
    }

    // ---- the blob ----

    #[test]
    fn the_empty_blob_is_a_new_account_and_a_stored_moment_round_trips() {
        assert_eq!(StoryState::decode(&[]), Some(StoryState::new_game()));
        assert_eq!(StoryState::new_game().moment, NEW_GAME_MOMENT);
        for moment in [0u16, 1, 6, 255, 256, 1600, u16::MAX] {
            let blob = at(moment);
            assert_eq!(blob.len(), 3);
            assert_eq!(StoryState::decode(&blob), Some(StoryState { moment }));
        }
        // What is not exactly version 1 is not read: other versions, other lengths, the old test blobs.
        for bad in [&[7u8, 7][..], &[2, 1, 0], &[1, 1], &[1, 1, 0, 0], &[0], &[0xAB; 64]] {
            assert_eq!(StoryState::decode(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn merging_keeps_the_further_moment_and_never_replaces_what_it_cannot_read() {
        assert_eq!(merge_progression(&[], &at(6)), at(6), "a new account takes the advanced story");
        assert_eq!(merge_progression(&at(6), &at(7)), at(7));
        assert_eq!(merge_progression(&at(7), &at(6)), at(7), "a late, older save cannot take the story back");
        assert_eq!(merge_progression(&at(6), &at(6)), at(6));
        assert_eq!(merge_progression(&at(6), &[]), at(6), "the default is not further than anything");
        assert_eq!(merge_progression(&[], &at(1)), Vec::<u8>::new(), "an equal story leaves the stored blob as it is");
        assert_eq!(merge_progression(&[7, 7], &at(9)), vec![7, 7], "an unreadable record is never overwritten");
        assert_eq!(merge_progression(&at(9), &[7, 7]), at(9));
    }

    // ---- phases ----

    #[test]
    fn the_intervals_between_the_literals_a_field_compares_are_its_phases() {
        // `moment == 1` cuts the axis at 1 and 2: the intervals are [0,1) [1,2) [2,..)
        let story = scan(&field_script(6, 0, 1, Some(0)), &mesh(13));
        assert_eq!(story.edges(), [1, 2]);
        let key = |m: u16| story.phase_key(&at(m));
        assert_eq!(key(1), Some(PhaseKey::default()), "the interval of the new-game moment is the empty key");
        assert_eq!(story.phase_key(&[]), Some(PhaseKey::default()), "a new account");
        assert_eq!(key(0), Some(PhaseKey(vec![PHASE_TAG, 0, 0])));
        assert_eq!(key(2), Some(PhaseKey(vec![PHASE_TAG, 2, 0])));
        assert_eq!(key(6), key(2), "6 and 2 look the same to this field");
        assert_eq!(key(9999), key(2));
        assert_eq!(story.phase_key(&[7, 7]), None, "a blob that is not ours has no phase of ours");
    }

    #[test]
    fn every_comparison_cuts_the_axis_where_its_answer_changes() {
        for (op, lit, cuts) in [(0u8, 5i16, vec![5u16, 6]), (1, 5, vec![5, 6]), (2, 5, vec![6]), (3, 5, vec![5]), (4, 5, vec![5]), (5, 5, vec![6])] {
            // a field with one comparison and nothing else
            let code = concat(&[ifsw_lit(2, 0, lit, op, 1), ret()]);
            let sec = SynthScript::default().entity("e", vec![(0, code)]).build();
            let story = scan(&sec, &mesh(1));
            assert_eq!(story.edges(), &cuts[..], "op {op}");
            // and two moments share a phase exactly when the comparison answers the same for both
            let cmp = Cmp::from_code(i32::from(op)).unwrap();
            let key = |x: u16| story.phase_key(&at(x)).unwrap();
            for m in 0..12u16 {
                let same_answer = cmp.holds(m, i32::from(lit)) == cmp.holds(m + 1, i32::from(lit));
                assert_eq!(key(m) == key(m + 1), same_answer, "op {op}, moments {m} and {}", m + 1);
            }
        }
        // the literal on the left means the comparison from the other side: `5 < moment` is `moment > 5`
        let flipped = vec![0x16, 0x02, 0x05, 0x00, 0x00, 0x00, 3, 1];
        let sec = SynthScript::default().entity("e", vec![(0, concat(&[flipped, ret()]))]).build();
        assert_eq!(scan(&sec, &mesh(1)).edges(), [6]);
    }

    #[test]
    fn a_field_that_never_compares_the_counter_has_one_phase_for_everybody() {
        let sec = SynthScript::default().entity("e", vec![(0, ret())]).build();
        let story = scan(&sec, &mesh(1));
        assert!(story.edges().is_empty() && story.point_count() == 0);
        for blob in [vec![], at(0), at(900)] {
            assert_eq!(story.phase_key(&blob), Some(PhaseKey::default()));
        }
    }

    // ---- story points ----

    #[test]
    fn stepping_onto_the_triangle_of_the_loop_advances_a_player_whose_story_matches() {
        let story = scan(&field_script(6, 0, 1, Some(0)), &mesh(13));
        assert_eq!(story.point_count(), 1);
        let points = story.points();
        let point = points.first().unwrap();
        assert_eq!(point.id, 0);
        let StoryTrigger::Region(corners) = &point.trigger else { panic!("{point:?}") };
        assert_eq!(corners.len(), 3);
        assert_eq!((corners[0].x, corners[0].y), (1200.0, 0.0), "triangle 12 of the mesh");
        assert!(point.label.contains("triangle 12") && point.label.contains("moment ==1 -> 6"), "{}", point.label);

        // A new account (empty blob) is at the moment the point needs: it advances to 6.
        let step = story.advance(&[], 0).expect("a new account advances");
        assert_eq!(step.progress, at(6));
        assert_eq!(step.summary, "moment 1 -> 6");
        // Once advanced it cannot do it again, and nobody at another moment can.
        assert_eq!(story.advance(&step.progress, 0), None);
        for m in [0u16, 2, 5, 6, 7, 1600] {
            assert_eq!(story.advance(&at(m), 0), None, "moment {m}");
        }
        // An unknown point and an unreadable blob do nothing.
        assert_eq!(story.advance(&[], 1), None);
        assert_eq!(story.advance(&[7, 7], 0), None);
    }

    #[test]
    fn a_lower_guard_such_as_less_than_works_the_same_and_a_story_never_goes_back() {
        // `moment < 7 -> 7` (the shape of the next field of the first mission)
        let story = scan(&field_script(7, 3, 7, Some(0)), &mesh(13));
        assert_eq!(story.point_count(), 1);
        assert_eq!(story.advance(&at(1), 0).unwrap().progress, at(7));
        assert_eq!(story.advance(&at(6), 0).unwrap().summary, "moment 6 -> 7");
        assert_eq!(story.advance(&at(7), 0), None);
        // A point whose value is not higher than the moment never lowers it, whatever the guard says.
        let story = scan(&field_script(3, 3, 7, Some(0)), &mesh(13));
        assert_eq!(story.advance(&at(5), 0), None, "5 -> 3 would be going back");
        assert_eq!(story.advance(&at(1), 0).unwrap().progress, at(3));
    }

    #[test]
    fn a_loop_that_is_not_the_pattern_makes_no_story_point() {
        // the entity the loop asks about is not bound to a playable character
        assert_eq!(scan(&field_script(6, 0, 1, None), &mesh(13)).point_count(), 0, "no PC: not the player");
        // the triangle is not in the walkmesh
        assert_eq!(scan(&field_script(6, 0, 1, Some(0)), &mesh(12)).point_count(), 0, "triangle 12 does not exist");
        // a guard the server cannot judge (a bit test on the counter instead of a comparison)
        assert_eq!(scan(&field_script(6, 6, 1, Some(0)), &mesh(13)).point_count(), 0);
    }

    #[test]
    fn a_write_with_no_region_is_not_a_story_point_whatever_it_is_guarded_by() {
        // main: IFSW Var[2][0] == 1 -> SETWORD Var[2][0] = 8 (the shape of a cutscene that just runs)
        let tail = setword(2, 0, 8);
        let guard = ifsw_lit(2, 0, 1, 0, u8::try_from(tail.len() + 1).unwrap());
        let jmpb = vec![0x12, u8::try_from(guard.len() + tail.len()).unwrap()];
        let code = concat(&[ret(), guard, tail, jmpb]);
        let sec = SynthScript::default().entity("runner", vec![(0, code)]).build();
        let story = scan(&sec, &mesh(13));
        assert_eq!(story.point_count(), 0, "nothing the server can see decides it");
        // ...but its comparison still shapes the phases
        assert_eq!(story.edges(), [1, 2]);
    }

    #[test]
    fn the_player_in_the_rule_is_any_playable_character_never_one_in_particular() {
        // The entity the loop asks about may stand for any character at all: the rule is "the player".
        for character in [0u8, 1, 5, 8, 10] {
            assert_eq!(scan(&field_script(6, 0, 1, Some(character)), &mesh(13)).point_count(), 1, "character {character}");
        }
    }
}
