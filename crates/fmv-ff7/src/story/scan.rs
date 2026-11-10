//! Reading the story points of a field out of its scripts (the rules are the parent module's docs).
//!
//! Every write the model carries (`SETWORD`/`SETBYTE` of `Var[2][0]` with a literal, `BITON`/`BITOFF` of a
//! tracked bit) is looked at in the places a script runs by itself or because the player walked
//! somewhere: script 0's init part and main part, and the walk-on slots of a `LINE` entity. For each write the
//! jumps that can skip it are classified ([`write_at`]); a write whose guards are all understood has a
//! trigger, the writes that share a trigger and nest their guards are one point ([`points_of`]).
//!
//! Two more things are read there. **A timer** (`STTIM` with literal operands) that the write's own block sets is the
//! countdown the step starts ([`timer_of_block`]). **A script that a main loop requests** (`REQ`, `REQEW`, `REQSW`
//! under guards the model can judge, nothing that waits or asks before it) writes the story on behalf of the loop: what
//! the requested script writes on every path from its start, before anything that ends it or waits, is the loop's own
//! entry point ([`requested_events`]; the first reactor's finale is one: the loop of a nameless entity in `nrthmk`
//! runs the director's cutscene script, which sets the moment).

use std::collections::{BTreeMap, BTreeSet};

use fmv_core::Pos;

use super::{
    compare_parts, guards_of, moment_compare, tracked_var, var_of, Cmp, Cond, Effect, Op, PointDef, PointSource, Trigger, COMPARES, MOMENT_ADDRESS,
    MOMENT_BANK,
};
use crate::field::Walkmesh;
use crate::script::{FieldScript, Instruction, INIT_SCRIPT, LINE_SLOTS, SCRIPTS_PER_ENTITY};

const OP_RET: u8 = 0x00;
const OP_RETTO: u8 = 0x07;
const OP_JMPB: u8 = 0x12;
const OP_JMPBL: u8 = 0x13;
const OP_MINIGAME: u8 = 0x20;
const OP_STTIM: u8 = 0x38;
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
pub(crate) struct LineOf {
    pub(crate) a: Pos,
    pub(crate) b: Pos,
    pub(crate) enabled: Option<bool>,
}

/// The init part of a script 0 (everything up to its first `RET`; all of it when it has none).
pub(crate) fn init_part(code: &[Instruction]) -> &[Instruction] {
    match code.iter().position(|i| i.opcode == OP_RET) {
        Some(first_ret) => code.get(..first_ret).unwrap_or(&[]),
        None => code,
    }
}

/// The bytes an init part sets to a literal on every path (a `SETBYTE`, or both bytes of a `SETWORD`): the
/// variables other scripts of the field compare things with (the triangle a loop waits for, the height a ladder
/// tests), as bank/address to byte.
pub(crate) fn init_bytes(init: &[Instruction]) -> BTreeMap<(u8, u8), u8> {
    let mut bytes = BTreeMap::new();
    for (k, ins) in init.iter().enumerate() {
        if !matches!(ins.mnemonic, "SETBYTE" | "SETWORD") || !guards_of(init, k).is_empty() {
            continue;
        }
        let (Some(var), Some(value)) = (ins.operand("var"), ins.operand("value")) else { continue };
        if var.bank == 0 || value.bank != 0 {
            continue;
        }
        let (bank, address) = var_of(var);
        if ins.mnemonic == "SETBYTE" {
            bytes.insert((bank, address), u8::try_from(value.value & 0xFF).unwrap_or(0));
        } else if let Some(next) = address.checked_add(1) {
            let [lo, hi] = i16::try_from(value.value).unwrap_or(0).to_le_bytes();
            bytes.insert((bank, address), lo);
            bytes.insert((bank, next), hi);
        }
    }
    bytes
}

/// What the scan of one entity's parts needs to know about the field.
struct Ctx<'a> {
    script: &'a FieldScript<'a>,
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
    /// A `BATTLE` the script starts right after the write (not run by the server).
    battle: Option<u16>,
    /// A `BATTLE` the script starts before the write, in the block the write is in.
    battle_first: Option<u16>,
    /// A timer the write's block sets, in seconds.
    countdown: Option<u32>,
}

/// The story points of a field: triangles first, then lines, then entry points, each in entity order.
pub(super) fn scan_points(script: &FieldScript, mesh: &Walkmesh) -> Vec<PointDef> {
    let players: BTreeSet<usize> = script.player_entities().into_iter().map(|(entity, _)| entity).collect();
    let triangle_vars = player_triangle_vars(script, &players);
    let triangle_sources = player_triangle_sources(script, &players);
    let (mut triangles, mut lines, mut enters, mut panels) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for entity in 0..script.entity_count() {
        let Ok(listing) = script.disassemble_partial(entity, INIT_SCRIPT) else { continue };
        let code = listing.instructions.as_slice();
        let (init, main) = match code.iter().position(|i| i.opcode == OP_RET) {
            Some(first_ret) => (code.get(..first_ret).unwrap_or(&[]), code.get(first_ret + 1..).unwrap_or(&[])),
            None => (code, &[][..]),
        };
        let ctx = Ctx { script, mesh, triangle_vars: &triangle_vars, constants: init_constants(init), line: line_of(init) };
        let source = |slot, start_offset, setup| PointSource { entity, slot, start_offset, setup, triangle_vars: Vec::new(), triangle_sources: Vec::new() };
        let attach = |mut points: Vec<PointDef>, source: PointSource, code: &[Instruction]| {
            for point in &mut points {
                let mut source = source.clone();
                if let Trigger::Triangle { triangle, .. } = point.trigger {
                    let inputs: BTreeSet<_> = code.iter().filter(|guard| triangle_test(guard, &ctx) == Some(triangle))
                        .filter_map(compare_parts).flat_map(|(left, right, _)| [left, right])
                        .filter(|operand| operand.bank != 0 && ctx.triangle_vars.contains(&var_of(operand)))
                        .map(var_of).collect();
                    // One authoritative trigger establishes one input. Multiple PC read-backs
                    // must not all be guessed to have the same triangle.
                    if inputs.len() == 1 {
                        for input in inputs {
                            if let Some(&actor) = triangle_sources.get(&input) {
                                source.triangle_vars.push(input);
                                source.triangle_sources.push((input.0, input.1, actor));
                            }
                        }
                    }
                }
                point.source = Some(source);
            }
            points
        };
        let mut found = attach(points_of(&events(init, Part::Init, &ctx)), source(INIT_SCRIPT, None, false), init);
        found.extend(attach(points_of(&events(main, Part::Main, &ctx)),
            source(INIT_SCRIPT, main.first().map(|instruction| instruction.offset), true), main));
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
                found.extend(attach(points_of(&events(&listing.instructions, Part::Line, &ctx)), source(slot, None, true), &listing.instructions));
            }
        }
        for point in found {
            // A panel that waits for OK also has the way back, which is not a step forward and which the text does not
            // show: a sibling point that runs the panel's own script (appended after every other kind, so no id moves).
            if let Trigger::Triangle { triangle, corners, wants_ok: true } = point.trigger {
                let vars = triangle_vars.iter().copied().collect();
                panels.push(PointDef {
                    trigger: Trigger::Panel { entity, triangle, corners, vars },
                    requires: Vec::new(),
                    effects: Vec::new(),
                    battle: None,
                    battle_first: None,
                    countdown: None,
                    source: point.source.clone(),
                });
            }
            match point.trigger {
                Trigger::Triangle { .. } => triangles.push(point),
                Trigger::Line { .. } => lines.push(point),
                Trigger::Enter => enters.push(point),
                // (talk points are added by `AreaStory::add_talk_points`, panels above: not found by the scan)
                Trigger::Talk { .. } | Trigger::Panel { .. } => {}
            }
        }
    }
    triangles.extend(lines);
    triangles.extend(enters);
    triangles.extend(panels);
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

/// Exact trigger inputs also retain their producer. Two playable entities writing one
/// destination do not establish that they stand on the same triangle.
fn player_triangle_sources(script: &FieldScript, players: &BTreeSet<usize>) -> BTreeMap<(u8, u8), usize> {
    let mut producers: BTreeMap<(u8, u8), BTreeSet<usize>> = BTreeMap::new();
    let mut destinations = BTreeSet::new();
    let canonical = |(bank, address): (u8, u8)|
        (super::canonical_persistent_bank(bank).unwrap_or(if bank == 6 { 5 } else { bank }), address);
    let mut seen = BTreeSet::new();
    for entity in 0..script.entity_count() {
        for slot in 0..SCRIPTS_PER_ENTITY {
            let Some(span) = script.span(entity, slot) else { continue };
            if !seen.insert(span.start) { continue; }
            let Ok(listing) = script.disassemble_partial(entity, slot) else { continue };
            for instruction in listing.instructions.iter().filter(|instruction| instruction.mnemonic == "GETAI") {
                let Some(var) = instruction.operand("var_triangle").filter(|operand| operand.bank != 0) else { continue };
                let Some(actor) = instruction.literal("entity").and_then(|actor| usize::try_from(actor).ok()) else { continue };
                let destination = var_of(var);
                destinations.insert(destination);
                producers.entry(canonical(destination)).or_default().insert(actor);
            }
        }
    }
    destinations.into_iter().filter_map(|input| {
        let actors = producers.get(&canonical(input))?;
        if actors.len() != 1 { return None; }
        actors.first().copied().filter(|actor| players.contains(actor)).map(|actor| (input, actor))
    }).collect()
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
pub(crate) fn line_of(init: &[Instruction]) -> Option<LineOf> {
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

/// Every write of the model in `code` that has a trigger, in script order, and the writes of the scripts it requests.
fn events(code: &[Instruction], part: Part, ctx: &Ctx) -> Vec<Write> {
    let mut writes: Vec<Write> = (0..code.len()).filter_map(|k| write_at(code, k, part, ctx)).collect();
    if part != Part::Line {
        writes.extend(requested_events(code, ctx));
    }
    writes
}

/// What the scripts that `code` (the init or the main part of an entity's script 0) REQUESTS write, as entry points.
///
/// A request (`REQ`, `REQEW`, `REQSW` with a literal entity and script) that the part makes on every path under guards
/// the model can judge (the moment against a literal, a carried bit: no triangle, no key, no party) makes the
/// requested script run, and what that script writes of the story on every path from its start (before a `RET`, a
/// `MAPJUMP`, a loop or an `ASK`, and under no `IF` of its own) is written by the requesting part: one [`Write`] per
/// write, in the requested script's order, all with the requesting guards and the trigger [`Trigger::Enter`] (the
/// part runs by itself from the field's first frame on, so does what it requests). Nothing before the request in the
/// requesting part may end it, loop or ask (a part that waits for something the server cannot see is not one). One
/// level of request is followed: a request in the requested script is not.
fn requested_events(code: &[Instruction], ctx: &Ctx) -> Vec<Write> {
    let mut writes = Vec::new();
    for (k, ins) in code.iter().enumerate() {
        let Some(target) = requested(ins) else { continue };
        let before = code.get(..k).unwrap_or(&[]);
        if before.iter().any(|i| STOPPERS.contains(&i.opcode) || is_wait_for_player(i)) {
            continue;
        }
        let guards = guards_of(code, k);
        let conds: Option<Vec<Cond>> = guards
            .iter()
            .map(|g| moment_compare(g).map(|(cmp, literal)| Cond::Moment(cmp, literal)).or_else(|| bit_cond(g)))
            .collect();
        let Some(conds) = conds else { continue };
        let Some(listing) = ctx.script.span(target.0, target.1).and_then(|_| ctx.script.disassemble_partial(target.0, target.1).ok()) else {
            continue;
        };
        let requested_code = listing.instructions.as_slice();
        for (k2, written) in requested_code.iter().enumerate() {
            let Some(op) = write_op(written) else { continue };
            let upto = requested_code.get(..k2).unwrap_or(&[]);
            if upto.iter().any(|i| STOPPERS.contains(&i.opcode) || is_wait_for_player(i)) || !guards_of(requested_code, k2).is_empty() {
                continue;
            }
            writes.push(Write {
                k,
                op,
                trigger: Trigger::Enter,
                chain: guards.iter().map(|g| g.offset).collect(),
                conds: conds.iter().copied().map(Some).collect(),
                battle: None,
                battle_first: None,
                countdown: None,
            });
        }
    }
    writes
}

/// An instruction after which a script may be waiting for something the server cannot see: a loop (a backward jump) or
/// a question for the player.
fn is_wait_for_player(ins: &Instruction) -> bool {
    matches!(ins.opcode, OP_JMPB | OP_JMPBL | OP_ASK)
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
        Trigger::Talk { .. } | Trigger::Panel { .. } => false,
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
    let battle_first = battle_first(code, k, &guards, ctx);
    let countdown = timer_of_block(code, k, &guards);
    Some(Write { k, op, trigger, chain, conds, battle, battle_first, countdown })
}

/// The timer (`STTIM` with literal hours, minutes and seconds) that the block of the write at `code[k]` sets, in seconds: an
/// `STTIM` that runs on every path that reaches the write, in the block the nearest guard opens (before the write or after
/// it). The first reactor's Guard Scorpion step sets the bomb's ten minutes in the same block as the moment.
fn timer_of_block(code: &[Instruction], k: usize, guards: &[&Instruction]) -> Option<u32> {
    let from = guards.iter().map(|g| g.end()).max().unwrap_or(0);
    let block_end = guards.iter().filter_map(|g| g.jump_target).min();
    code.iter().enumerate().filter(|(j, i)| *j != k && i.opcode == OP_STTIM && i.offset >= from && block_end.is_none_or(|end| i.offset < end)).find_map(|(j, i)| {
        if !guards_of(code, j).iter().all(|g| guards.iter().any(|w| w.offset == g.offset)) {
            return None;
        }
        let part = |name: &str, unit: u32| i.literal(name).and_then(|v| u32::try_from(v).ok()).and_then(|v| v.checked_mul(unit));
        part("hours", 3600)?.checked_add(part("minutes", 60)?)?.checked_add(part("seconds", 1)?)
    })
}

/// How deep a request chain is followed for a battle or a write (a script requests a script requests a script ...).
const REQUEST_DEPTH: u8 = 4;

/// The scripts a request instruction (`REQ`, `REQSW`, `REQEW`) names with literal operands, as `(entity, slot)`.
fn requested(ins: &Instruction) -> Option<(usize, usize)> {
    if !matches!(ins.opcode, 0x01..=0x03) {
        return None;
    }
    Some((usize::try_from(ins.literal("entity")?).ok()?, usize::try_from(ins.literal("script")?).ok()?))
}

/// What `wanted` finds first in script `slot` of `entity` or in a script it requests (to `depth`). A script already
/// looked at is not looked at again.
fn find_in_requests<T>(
    script: &FieldScript,
    (entity, slot): (usize, usize),
    depth: u8,
    seen: &mut BTreeSet<usize>,
    wanted: &dyn Fn(&Instruction) -> Option<T>,
) -> Option<T> {
    let span = script.span(entity, slot)?;
    if depth == 0 || !seen.insert(span.start) {
        return None;
    }
    let listing = script.disassemble_partial(entity, slot).ok()?;
    for ins in &listing.instructions {
        if let Some(found) = wanted(ins) {
            return Some(found);
        }
        if let Some(target) = requested(ins) {
            if let Some(found) = find_in_requests(script, target, depth.saturating_sub(1), seen, wanted) {
                return Some(found);
            }
        }
    }
    None
}

/// The literal battle a `BATTLE` instruction starts.
fn battle_of(ins: &Instruction) -> Option<u16> {
    if ins.mnemonic != "BATTLE" {
        return None;
    }
    u16::try_from(ins.literal("battle")?).ok()
}

/// A `BATTLE` the script starts before the write at `code[k]`, in the block the write is in, directly or in a script that
/// block requests: the fight whose win the story step comes after (the guard scorpion: a director's loop asks the
/// characters' scripts to run the cutscene, which fights, and the moment is set when they return). Only what runs on
/// every path to the write counts: an instruction under a condition the write is not under may not run.
fn battle_first(code: &[Instruction], k: usize, guards: &[&Instruction], ctx: &Ctx) -> Option<u16> {
    // The block starts after the innermost jump that skips the write.
    let from = guards.iter().map(|g| g.end()).max().unwrap_or(0);
    let mut seen = BTreeSet::new();
    for (j, ins) in code.iter().enumerate().take(k).filter(|(_, i)| i.offset >= from) {
        if !guards_of(code, j).iter().all(|g| guards.iter().any(|w| w.offset == g.offset)) {
            continue;
        }
        if let Some(battle) = battle_of(ins) {
            return Some(battle);
        }
        if let Some(target) = requested(ins) {
            if let Some(battle) = find_in_requests(ctx.script, target, REQUEST_DEPTH, &mut seen, &battle_of) {
                return Some(battle);
            }
        }
    }
    None
}

/// Whether the talk script of character `entity` (slot 1), or a script it requests, writes the story: the moment, or a
/// bit of a tracked variable. Such a character gets a talk point (what the talk does is then run, not read: see
/// [`super::Trigger::Talk`]).
pub(super) fn talk_can_write_the_story(script: &FieldScript, entity: usize) -> bool {
    let writes = |ins: &Instruction| write_op(ins).map(|_| ());
    find_in_requests(script, (entity, crate::script::TALK_SCRIPT), REQUEST_DEPTH, &mut BTreeSet::new(), &writes).is_some()
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
        // A point that wants a bit set that nothing in the model sets (the countdown's timeout flag: the loop of
        // `nrthmk` that plays the way a late party's escape goes wrong) can never apply: it is not offered, so it does not
        // take the id of a point that can.
        if !requires.iter().all(|c| c.possible()) {
            continue;
        }
        let mut effects = Vec::new();
        let (mut battle, mut battle_first, mut countdown) = (None, None, None);
        for member in members {
            let extra: Vec<Cond> = member.conds.get(base.conds.len()..).unwrap_or(&[]).iter().flatten().copied().collect();
            if !extra.iter().all(|c| c.possible()) || !member.op.possible() {
                continue;
            }
            // A write nothing conditions would set the story on every visit: a script that does not state when
            // is not read.
            if requires.is_empty() && extra.is_empty() {
                continue;
            }
            battle = battle.or(member.battle);
            battle_first = battle_first.or(member.battle_first);
            countdown = countdown.or(member.countdown);
            effects.push(Effect { requires: extra, op: member.op });
        }
        // A point that could only clear bits moves nothing forward.
        if effects.iter().any(|e| matches!(e.op, Op::Moment(_) | Op::Bit { on: true, .. })) {
            points.push(PointDef { trigger: base.trigger.clone(), requires, effects, battle, battle_first, countdown, source: None });
        }
    }
    points
}
