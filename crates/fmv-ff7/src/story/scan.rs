//! Reading the story points of a field out of its scripts (the rules are the parent module's docs).
//!
//! Every write the model carries (`SETWORD`/`SETBYTE` of `Var[2][0]` with a literal, `BITON`/`BITOFF` of a
//! tracked bit) is looked at in the places a script runs by itself or because the player walked
//! somewhere: script 0's init part and main part, and the walk-on slots of a `LINE` entity. For each write the
//! jumps that can skip it are classified ([`write_at`]); a write whose guards are all understood has a
//! trigger, the writes that share a trigger and nest their guards are one point ([`points_of`]).

use std::collections::{BTreeMap, BTreeSet};

use fmv_core::Pos;

use super::{
    compare_parts, guards_of, moment_compare, tracked_var, var_of, Cmp, Cond, Effect, Op, PointDef, Trigger, COMPARES, MOMENT_ADDRESS,
    MOMENT_BANK,
};
use crate::field::Walkmesh;
use crate::script::{FieldScript, Instruction, INIT_SCRIPT, LINE_SLOTS, SCRIPTS_PER_ENTITY};

const OP_RET: u8 = 0x00;
const OP_RETTO: u8 = 0x07;
const OP_JMPB: u8 = 0x12;
const OP_JMPBL: u8 = 0x13;
const OP_MINIGAME: u8 = 0x20;
const OP_IFKEY: u8 = 0x30;
const OP_IFKEYON: u8 = 0x31;
const OP_ASK: u8 = 0x48;
const OP_MAPJUMP: u8 = 0x60;
const OP_LINE: u8 = 0xD0;
const OP_LINON: u8 = 0xD1;
const OP_GAMEOVER: u8 = 0xFF;

/// What ends a script or takes the party out of the field: a write that comes after one of these is not one the
/// script is sure to reach.
const STOPPERS: [u8; 5] = [OP_RET, OP_RETTO, OP_MINIGAME, OP_MAPJUMP, OP_GAMEOVER];

/// The OK button's bit in a key mask (`IFKEYON 0x20` is "OK pressed"; the elevator's panel asks for 0x220).
const KEY_OK: i32 = 0x20;

/// Which part of an entity's scripts a write is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part {
    /// Script 0 up to its first `RET`: runs once when the field loads.
    Init,
    /// Script 0 after its first `RET`: runs from the first frame on.
    Main,
    /// A walk-on slot of a line entity.
    Line,
}

/// The line a `LINE` entity watches and whether it starts enabled.
#[derive(Debug, Clone, Copy)]
struct LineOf {
    a: Pos,
    b: Pos,
    enabled: Option<bool>,
}

/// What the scan of one entity's parts needs to know about the field.
struct Ctx<'a> {
    mesh: &'a Walkmesh,
    /// Variables that a `GETAI` fills with the triangle of a playable character's entity (and nothing else
    /// fills with one of another entity).
    triangle_vars: &'a BTreeSet<(u8, u8)>,
    /// What the entity's init part sets to a literal on every path.
    constants: BTreeMap<(u8, u8), i32>,
    line: Option<LineOf>,
}

/// One write of the model, read out of a script with the conditions around it.
struct Write {
    /// Index of the instruction in its part (the order the script makes the writes in).
    k: usize,
    op: Op,
    trigger: Trigger,
    /// The offsets of the jumps that guard it, outermost first, and what each says (`None`: the jump is the
    /// trigger itself, the test of the triangle or of the OK button).
    chain: Vec<usize>,
    conds: Vec<Option<Cond>>,
    battle: Option<u16>,
}

/// The story points of a field: triangles first, then lines, then entry points, each in entity order.
pub(super) fn scan_points(script: &FieldScript, mesh: &Walkmesh) -> Vec<PointDef> {
    let players: BTreeSet<usize> = script.player_entities().into_iter().map(|(entity, _)| entity).collect();
    let triangle_vars = player_triangle_vars(script, &players);
    let (mut triangles, mut lines, mut enters) = (Vec::new(), Vec::new(), Vec::new());
    for entity in 0..script.entity_count() {
        let Ok(listing) = script.disassemble_partial(entity, INIT_SCRIPT) else { continue };
        let code = listing.instructions.as_slice();
        let (init, main) = match code.iter().position(|i| i.opcode == OP_RET) {
            Some(first_ret) => (code.get(..first_ret).unwrap_or(&[]), code.get(first_ret + 1..).unwrap_or(&[])),
            None => (code, &[][..]),
        };
        let ctx = Ctx { mesh, triangle_vars: &triangle_vars, constants: init_constants(init), line: line_of(init) };
        let mut found = points_of(&events(init, Part::Init, &ctx));
        found.extend(points_of(&events(main, Part::Main, &ctx)));
        // A line entity's walk-on slots (not `[OK]`), when the line starts enabled; slots that share an entry
        // point are one script.
        if ctx.line.is_some_and(|l| l.enabled != Some(false)) {
            let mut seen = BTreeSet::new();
            for slot in LINE_SLOTS.filter(|&s| s != 1) {
                let slot = usize::from(slot);
                let Some(span) = script.span(entity, slot) else { continue };
                if !seen.insert(span.start) {
                    continue;
                }
                let Ok(listing) = script.disassemble_partial(entity, slot) else { continue };
                found.extend(points_of(&events(&listing.instructions, Part::Line, &ctx)));
            }
        }
        for point in found {
            match point.trigger {
                Trigger::Triangle { .. } => triangles.push(point),
                Trigger::Line { .. } => lines.push(point),
                Trigger::Enter => enters.push(point),
            }
        }
    }
    triangles.extend(lines);
    triangles.extend(enters);
    triangles
}

/// The variables that hold the triangle of a playable character: filled by a `GETAI` of an entity with a `PC`
/// somewhere in the field (the director's loop, or the same loop that tests it), and by no `GETAI` of any
/// other entity.
fn player_triangle_vars(script: &FieldScript, players: &BTreeSet<usize>) -> BTreeSet<(u8, u8)> {
    let (mut of_players, mut of_others) = (BTreeSet::new(), BTreeSet::new());
    let mut seen = BTreeSet::new();
    for entity in 0..script.entity_count() {
        for slot in 0..SCRIPTS_PER_ENTITY {
            let Some(span) = script.span(entity, slot) else { continue };
            if !seen.insert(span.start) {
                continue;
            }
            let Ok(listing) = script.disassemble_partial(entity, slot) else { continue };
            for ins in listing.instructions.iter().filter(|i| i.mnemonic == "GETAI") {
                let Some(var) = ins.operand("var_triangle").filter(|v| v.bank != 0) else { continue };
                let of_player = ins.literal("entity").and_then(|e| usize::try_from(e).ok()).is_some_and(|e| players.contains(&e));
                let key = var_of(var);
                if of_player {
                    of_players.insert(key);
                } else {
                    of_others.insert(key);
                }
            }
        }
    }
    of_players.difference(&of_others).copied().collect()
}

/// What an init part sets to a literal on every path (a variable the main loop then compares the triangle with).
fn init_constants(init: &[Instruction]) -> BTreeMap<(u8, u8), i32> {
    let mut constants = BTreeMap::new();
    for (k, ins) in init.iter().enumerate() {
        if !matches!(ins.mnemonic, "SETBYTE" | "SETWORD") || !guards_of(init, k).is_empty() {
            continue;
        }
        if let (Some(var), Some(value)) = (ins.operand("var"), ins.operand("value")) {
            if var.bank != 0 && value.bank == 0 {
                constants.insert(var_of(var), value.value);
            }
        }
    }
    constants
}

/// The line of a line entity: the first `LINE` of its init part with literal operands, and whether the last
/// unconditional `LINON` of the init part switches it on (`None`: the init part never says).
fn line_of(init: &[Instruction]) -> Option<LineOf> {
    let line = init.iter().find(|i| i.opcode == OP_LINE)?;
    let coordinate = |name: &str| line.literal(name).and_then(|v| i16::try_from(v).ok()).map(f32::from);
    let a = Pos::new(coordinate("x1")?, coordinate("y1")?, coordinate("z1")?);
    let b = Pos::new(coordinate("x2")?, coordinate("y2")?, coordinate("z2")?);
    let enabled = init
        .iter()
        .enumerate()
        .filter(|&(k, i)| i.opcode == OP_LINON && guards_of(init, k).is_empty())
        .filter_map(|(_, i)| i.literal("enabled"))
        .next_back()
        .map(|v| v != 0);
    Some(LineOf { a, b, enabled })
}

/// Every write of the model in `code` that has a trigger, in script order.
fn events(code: &[Instruction], part: Part, ctx: &Ctx) -> Vec<Write> {
    (0..code.len()).filter_map(|k| write_at(code, k, part, ctx)).collect()
}

/// The write of the model that `code[k]` makes, with its trigger and conditions, if it makes one and every jump
/// that can skip it is understood.
fn write_at(code: &[Instruction], k: usize, part: Part, ctx: &Ctx) -> Option<Write> {
    let ins = code.get(k)?;
    let op = write_op(ins)?;
    let before = code.get(..k).unwrap_or(&[]);
    // Nothing that ends the script or leaves the field may come first: the write would not be sure to be reached.
    if before.iter().any(|i| STOPPERS.contains(&i.opcode)) {
        return None;
    }
    let (mut chain, mut conds) = (Vec::new(), Vec::new());
    let (mut triangle, mut wants_ok) = (None, false);
    let guards = guards_of(code, k);
    for guard in &guards {
        chain.push(guard.offset);
        let cond = moment_compare(guard).map(|(cmp, literal)| Cond::Moment(cmp, literal)).or_else(|| bit_cond(guard));
        if cond.is_some() {
            conds.push(cond);
            continue;
        }
        conds.push(None);
        // A jump the server cannot judge makes the whole write no point.
        if let Some(found) = triangle_test(guard, ctx) {
            if triangle.replace(found).is_some() {
                return None; // two region tests: not the pattern
            }
        } else if is_ok_key(guard) {
            wants_ok = true;
        } else {
            return None;
        }
    }
    let trigger = match (part, triangle) {
        (Part::Main, Some(triangle)) => {
            let verts = ctx.mesh.triangles.get(triangle)?;
            let corner = |i: usize| verts.get(i).map(|v| Pos::new(f32::from(v.x), f32::from(v.y), f32::from(v.z)));
            Trigger::Triangle { triangle, corners: [corner(0)?, corner(1)?, corner(2)?], wants_ok }
        }
        // A region test anywhere else is not the player standing somewhere, and a key is tolerated nowhere else.
        (_, Some(_)) => return None,
        (_, None) if wants_ok => return None,
        (Part::Line, None) => {
            let line = ctx.line?;
            Trigger::Line { a: line.a, b: line.b }
        }
        (Part::Init | Part::Main, None) => Trigger::Enter,
    };
    // What the player would have to answer, or a loop that waits, before the write: not a thing that runs by itself.
    let asks = before.iter().any(|i| i.opcode == OP_ASK);
    let loops = before.iter().any(|i| matches!(i.opcode, OP_JMPB | OP_JMPBL));
    let by_itself = match trigger {
        Trigger::Triangle { .. } => false,
        Trigger::Line { .. } => asks,
        Trigger::Enter => asks || loops,
    };
    if by_itself {
        return None;
    }
    // A battle the script starts inside the same guarded block, before the nearest guard's target.
    let block_end = guards.iter().filter_map(|g| g.jump_target).min();
    let battle = code
        .iter()
        .skip(k + 1)
        .take_while(|i| block_end.is_none_or(|end| i.offset < end))
        .find(|i| i.mnemonic == "BATTLE")
        .and_then(|i| i.literal("battle"))
        .and_then(|b| u16::try_from(b).ok());
    Some(Write { k, op, trigger, chain, conds, battle })
}

/// What the instruction writes that the model carries: the moment (a literal into `Var[2][0]`), or a tracked bit.
fn write_op(ins: &Instruction) -> Option<Op> {
    match ins.mnemonic {
        "SETBYTE" | "SETWORD" => {
            let (var, value) = (ins.operand("var")?, ins.operand("value")?);
            (var.bank != 0 && value.bank == 0 && var_of(var) == (MOMENT_BANK, MOMENT_ADDRESS)).then_some(())?;
            Some(Op::Moment(u16::try_from(value.value).ok()?))
        }
        "BITON" | "BITOFF" => {
            let (var, bit) = (ins.operand("var")?, ins.literal("bit")?);
            let (bank, address) = var_of(var);
            let bit = u8::try_from(bit).ok()?;
            let mask = 1u8.checked_shl(u32::from(bit))?;
            (var.bank != 0 && tracked_var(bank, address)?.bits & mask != 0).then_some(())?;
            Some(Op::Bit { bank, address, bit, on: ins.mnemonic == "BITON" })
        }
        _ => None,
    }
}

/// `Var[b][a] has bit n` / `lacks bit n` of a tracked bit (the codes 9 and 10 of the comparisons).
fn bit_cond(ins: &Instruction) -> Option<Cond> {
    if !COMPARES.contains(&ins.mnemonic) {
        return None;
    }
    let set = match ins.operand("op")?.value {
        9 => true,
        10 => false,
        _ => return None,
    };
    let (left, right) = (ins.operand("left")?, ins.operand("right")?);
    if left.bank == 0 || right.bank != 0 {
        return None;
    }
    let (bank, address) = var_of(left);
    let bit = u8::try_from(right.value).ok()?;
    let mask = 1u8.checked_shl(u32::from(bit))?;
    (tracked_var(bank, address)?.bits & mask != 0).then_some(Cond::Bit { bank, address, bit, set })
}

/// The walkmesh triangle that `guard` tests the player stands on: a variable filled with the triangle of a
/// playable character equals a literal, or equals a variable the init part set to a literal.
fn triangle_test(guard: &Instruction, ctx: &Ctx) -> Option<usize> {
    let (left, right, op) = compare_parts(guard)?;
    if op != Cmp::Eq {
        return None;
    }
    let against_literal = |var: &crate::script::Operand, literal: &crate::script::Operand| {
        (var.bank != 0 && literal.bank == 0 && ctx.triangle_vars.contains(&var_of(var))).then(|| usize::try_from(literal.value).ok()).flatten()
    };
    let against_constant = |var: &crate::script::Operand, constant: &crate::script::Operand| {
        if var.bank == 0 || constant.bank == 0 || !ctx.triangle_vars.contains(&var_of(var)) {
            return None;
        }
        usize::try_from(*ctx.constants.get(&var_of(constant))?).ok()
    };
    against_literal(left, right)
        .or_else(|| against_literal(right, left))
        .or_else(|| against_constant(left, right))
        .or_else(|| against_constant(right, left))
}

/// `IFKEY` / `IFKEYON` with the OK button among the keys: "the player presses OK".
fn is_ok_key(guard: &Instruction) -> bool {
    matches!(guard.opcode, OP_IFKEY | OP_IFKEYON) && guard.literal("keys").is_some_and(|keys| keys & KEY_OK != 0)
}

/// The story points of the writes of one part: the writes that share a trigger and whose guards nest (one's
/// chain starts with the other's) are one point, the shorter chain being the point's own conditions and what the
/// longer adds being its write's. The writes of a point are judged together on the story as it was, which is
/// why the elevator's ride is one point: its moment write sits under the bit test its bit write also sits under.
fn points_of(writes: &[Write]) -> Vec<PointDef> {
    // Shortest chains first, so that the first write of a point has the chain every later one starts with.
    let mut order: Vec<&Write> = writes.iter().collect();
    order.sort_by_key(|w| (w.chain.len(), w.k));
    let mut groups: Vec<(&Write, Vec<&Write>)> = Vec::new();
    for write in order {
        match groups.iter_mut().find(|(base, _)| base.trigger == write.trigger && write.chain.starts_with(&base.chain)) {
            Some((_, members)) => members.push(write),
            None => groups.push((write, vec![write])),
        }
    }
    groups.sort_by_key(|(base, _)| base.k);
    let mut points = Vec::new();
    for (base, mut members) in groups {
        members.sort_by_key(|w| w.k);
        let requires: Vec<Cond> = base.conds.iter().flatten().copied().collect();
        let mut effects = Vec::new();
        let mut battle = None;
        for member in members {
            let extra: Vec<Cond> = member.conds.get(base.conds.len()..).unwrap_or(&[]).iter().flatten().copied().collect();
            // A write nothing conditions would set the story on every visit: a script that does not state when
            // is not read.
            if requires.is_empty() && extra.is_empty() {
                continue;
            }
            battle = battle.or(member.battle);
            effects.push(Effect { requires: extra, op: member.op });
        }
        // A point that could only clear bits moves nothing forward.
        if effects.iter().any(|e| matches!(e.op, Op::Moment(_) | Op::Bit { on: true, .. })) {
            points.push(PointDef { trigger: base.trigger.clone(), requires, effects, battle });
        }
    }
    points
}
