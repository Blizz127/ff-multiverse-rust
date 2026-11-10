//! Survey of what the scene interpreter makes of the scripts that host the story points of the first reactor mission
//! (needs the owner's data; skips otherwise). Prints field names, entity names, opcode names, dialog NUMBERS, counts and
//! coordinates only (never game text), and writes nothing.
//!
//! ```text
//! FMV_FF7_DATA=<.../data> cargo run --offline -q -p fmv-ff7 --example point_scene_survey -- [FIELD...] [--brief] [--verbose] [--at MOMENT]
//! ```
//!
//! Without field names it walks the ten reviewed fields. For each field it
//!
//! 1. lists every instruction that writes the story moment (`Var[2][0]`, and its high byte) or the reactor's state byte
//!    (`Var[1][225]`), with entity, script slot and offset (slots that share one entry point are one script), and which
//!    scripts request it (`REQ`, `REQSW`, `REQEW`);
//! 2. checks that the stage setup (`script 0` of every entity, run quietly up to the first `RET` it executes) does stop at
//!    the first `RET`: no init part jumps over its own `RET`, and the setup executes no more instructions than the init
//!    parts hold;
//! 3. for each story point of [`SPECS`] (the first reactor mission's points, with the moments they apply at) builds
//!    throwaway [`SceneDef`]s and runs each with [`run_to_end`] (the outcome) and by driving a [`ScenePlay`] (the windows,
//!    who opened them, who acts):
//!    * `host`: what the field would run for that write (`StartMain` when the write is in the main part of script 0,
//!      else `Await` of the script that holds it);
//!    * `all-mains`: the main part of EVERY entity (the director, the entity called `dir`, first), so that the loops run
//!      beside the host as they do after the field loads; a host that is not a main part is started first (see
//!      `all_mains_root`: it shares a priority slot with the main part of its own entity);
//!    * `via`: for a write in a script that another script requests (a talk script that requests an actor's script),
//!      the requesting script (slot 1 and up);
//!    * `probe`: for a main part that waits on a variable a director's `GETAI` loop fills (the elevator's panel), the host
//!      told that the variable already holds the triangle (`SceneDef::given`), to see what it does next. A probe, not a
//!      run of the field;
//!    * for a point that waits for the player on a walkmesh triangle (region, press) every run is made twice: with the
//!      player's entity (the one `GETAI` reads, that has a `PC`) standing on the triangle's centroid, and as a CONTROL
//!      without. An unresolved `GETAI` leaves its variable 0, which another unset variable then equals: a write that
//!      happens in the control too is not gated by the triangle, and an expected write in a run that executed a `GETAI`
//!      and has `unresolved > 0` is printed as `YES?` (unverified), not `YES` (the other unresolved reads, a turn or a
//!      walk towards an entity nobody placed, set no variable; they are counted in the `unres` column all the same).
//!
//! Every setup is the init part of every entity's script 0. A run that ends in `Budget` or `Broken`, or reads something it
//! cannot resolve, is printed as it is: nothing here steers a run towards the expected write. The expected write is judged
//! on the FINAL value (`Writes::moment`, the tracked byte), so a later write over an earlier one hides the earlier one; a
//! write that the setup itself made (no window can open there) is marked `SETUP`. The moments of a point are the ones it
//! applies at (`<N`: N-1 and 1, `>=27`: 27); moments that give the very same run are printed once (`m=6+1`).
//!
//! The windows are printed as `entity#dialog` (a dialog NUMBER of the field's table), `?[a-b]` after one that asks (the
//! absolute lines of the choices), `note@entity` for the note the interpreter adds after a gift it does not give.
//! Every answer of an asking window is tried (at most 16 runs per case); the distinct outcomes are printed.
//!
//! What a run cannot show, whatever the script says (the interpreter's own limits, seen here): nobody presses a key
//! (`IFKEY`, `IFKEYON`, `IFKEYOFF` are false); a `MOVE` leaves the player's triangle unknown (`GETAI` is then unresolved);
//! a `BATTLE`, `MAPJUMP`, `MINIGAME` or `GAMEOVER` ends the scene; a loop that changes a variable or starts a script every
//! lap never parks (`Budget`); `unknown_persistent_reads` also lists a persistent variable the run only SETS (the
//! interpreter reads it to see whether the write changes anything), which is why it marks the ones a script writes (`*`).
#[path = "../tests/script_common.rs"]
mod common;

use common::{data_dir, flevel_path, load_field};
use fmv_ff7::field::{section, FieldFile, ModelLoader, Walkmesh};
use fmv_ff7::lgp::LgpArchive;
use fmv_ff7::script::dialogue::{End, MessageOrigin};
use fmv_ff7::script::scene::{run_to_end, CastMember, Given, Look, Outcome, SceneDef, SceneJump, ScenePlay, ScriptRef, Spot, Stage, Stats, Step, Then, Trigger, Writes};
use fmv_ff7::script::{parse_model_list, triangle_contains_xy, EntityInit, FieldScript, Instruction, SCRIPTS_PER_ENTITY};
use fmv_ff7::story::StoryState;
use std::collections::{BTreeMap, BTreeSet};

/// The seed of the script's `RANDOM` (every run is the same run).
const SEED: u32 = 1;

/// The ten reviewed fields of the first reactor mission (`fmv_ff7::REVIEWED_FIELDS`).
const FIELDS: [&str; 10] = ["md1stin", "md1_1", "md1_2", "nrthmk", "nmkin_1", "elevtr1", "nmkin_2", "nmkin_3", "nmkin_4", "nmkin_5"];

/// Most runs one case may make to try the answers of its asking windows.
const MAX_ANSWER_RUNS: usize = 16;

/// The opcodes the coordinator asked to see when the interpreter skips (or, for `BATTLE`, ends the scene at) them.
const LISTED: [&str; 17] = [
    "PRTYP", "PRTYM", "PRTYE", "HMPMAX1", "HMPMAX2", "HMPMAX3", "MHMMX", "GOLDu", "GOLDd", "MENU", "MMBud", "SPECIAL", "STITM", "BATTLE", "BTLON", "MOVIE", "PMVIE",
];

/// Opcodes the interpreter skips that the coordinator's list does not name, though they move or request the party's own
/// characters (`PREQ`, `JOIN`, ...) or give items and materia (`SMTRA`, ...): my addition.
const ALSO_SKIPPED: [&str; 12] = ["PREQ", "PRQSW", "PRQEW", "JOIN", "SPLIT", "PMOVA", "SMTRA", "DMTRA", "CMTRA", "DLITM", "CKITM", "CHGLD"];

/// Mnemonics the interpreter always counts as unknown in a stage setup and that change nothing a scene shows (the model
/// and the player binding of an entity).
const BENIGN_UNKNOWN: [&str; 2] = ["CHAR", "PC"];

/// What a story point's write is looked for by.
#[derive(Clone, Copy)]
enum Locate {
    /// A write of this literal to the moment.
    Moment(i32),
    /// `BITON` of this bit of `Var[1][225]`.
    Bit(i32),
}

/// What a point is expected to do.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Expect {
    Moment(u16),
    /// A bit of `Var[1][225]` on.
    Bit(u8),
}

/// One story point of the coordinator's list.
struct Spec {
    field: &'static str,
    /// Short id for the table.
    id: &'static str,
    label: &'static str,
    locate: Locate,
    /// The moments at which the point applies (`<N`: N-1 and 1; `>=27`: 27).
    moments: &'static [u16],
    /// The triangle the player has to stand on (region and press points).
    triangle: Option<u16>,
    expect: &'static [Expect],
}

const SPECS: &[Spec] = &[
    Spec { field: "md1stin", id: "md1stin/1", label: "[entry] moment ==0 -> 1", locate: Locate::Moment(1), moments: &[0], triangle: None, expect: &[Expect::Moment(1)] },
    Spec { field: "md1stin", id: "md1stin/6", label: "[region] triangle 12: moment ==1 -> 6 (then battle 300)", locate: Locate::Moment(6), moments: &[1], triangle: Some(12), expect: &[Expect::Moment(6)] },
    Spec { field: "md1_1", id: "md1_1/7", label: "[region] triangle 62: moment <7 -> 7", locate: Locate::Moment(7), moments: &[6, 1], triangle: Some(62), expect: &[Expect::Moment(7)] },
    Spec { field: "md1_2", id: "md1_2/8", label: "[entry] moment <8 -> 8", locate: Locate::Moment(8), moments: &[7, 1], triangle: None, expect: &[Expect::Moment(8)] },
    Spec { field: "nrthmk", id: "nrthmk/10", label: "[entry] moment <10 -> 10", locate: Locate::Moment(10), moments: &[9, 1], triangle: None, expect: &[Expect::Moment(10)] },
    Spec { field: "nrthmk", id: "nrthmk/33", label: "[entry] moment >=27, Var[1][225] bit 7 clear -> 33 (+ bit 0 on)", locate: Locate::Moment(33), moments: &[27], triangle: None, expect: &[Expect::Moment(33), Expect::Bit(0)] },
    Spec { field: "nmkin_1", id: "nmkin_1/11", label: "[line] moment <11 -> 11", locate: Locate::Moment(11), moments: &[10, 1], triangle: None, expect: &[Expect::Moment(11)] },
    Spec { field: "elevtr1", id: "elevtr1/12", label: "[press] triangle 8 (OK): bit 0 clear -> on, moment <12 -> 12", locate: Locate::Moment(12), moments: &[11, 1], triangle: Some(8), expect: &[Expect::Moment(12), Expect::Bit(0)] },
    Spec { field: "nmkin_2", id: "nmkin_2/13", label: "[entry] moment <13 -> 13", locate: Locate::Moment(13), moments: &[12, 1], triangle: None, expect: &[Expect::Moment(13)] },
    Spec { field: "nmkin_3", id: "nmkin_3/14", label: "[talk] entity 4: moment <14 -> 14", locate: Locate::Moment(14), moments: &[13, 1], triangle: None, expect: &[Expect::Moment(14)] },
    Spec { field: "nmkin_3", id: "nmkin_3/b5", label: "[talk] entity 4 at moment >=27: Var[1][225] bit 5 on", locate: Locate::Bit(5), moments: &[27], triangle: None, expect: &[Expect::Bit(5)] },
    Spec { field: "nmkin_5", id: "nmkin_5/15", label: "[talk] entity 6: moment -> 15", locate: Locate::Moment(15), moments: &[14], triangle: None, expect: &[Expect::Moment(15)] },
    Spec { field: "nmkin_5", id: "nmkin_5/27", label: "[region] triangle 2: moment <27 -> 27 (battle 324 first)", locate: Locate::Moment(27), moments: &[26, 1], triangle: Some(2), expect: &[Expect::Moment(27)] },
    // Found by the scan and by `world_survey` (later moments of the same director), not in the coordinator's list.
    Spec { field: "nmkin_5", id: "nmkin_5/123", label: "(extra) [entry] moment ==120 -> 123", locate: Locate::Moment(123), moments: &[120], triangle: None, expect: &[Expect::Moment(123)] },
    Spec { field: "nmkin_5", id: "nmkin_5/127", label: "(extra) [region] triangle 2: moment ==123 -> 127", locate: Locate::Moment(127), moments: &[123], triangle: Some(2), expect: &[Expect::Moment(127)] },
];

#[derive(Default)]
struct Opts {
    brief: bool,
    verbose: bool,
    at: Option<u16>,
}

// ---- the static scan ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum What {
    /// A write to the moment's byte (`high`: its second byte). `value` is the literal of a `SETBYTE`/`SETWORD`.
    Moment { value: Option<i32>, high: bool },
    /// `BITON`/`BITOFF`/`BITXOR` of a bit of `Var[1][225]` (`bit` is its literal).
    Bit { op: &'static str, bit: Option<i32> },
    /// Any other write to `Var[1][225]`.
    Byte,
}

/// One instruction that writes the story, with the script that holds it.
#[derive(Clone, Debug)]
struct Site {
    entity: usize,
    /// The lowest slot that reaches the script.
    slot: usize,
    /// How many slots share its entry point.
    shared: usize,
    offset: usize,
    text: String,
    what: What,
    /// In the main part of script 0 (after its first `RET`).
    in_main: bool,
}

/// A script that requests another script (`REQ`, `REQSW`, `REQEW`).
#[derive(Clone, Debug)]
struct Requester {
    entity: usize,
    slot: usize,
    offset: usize,
    op: &'static str,
    in_main: bool,
}

struct Info<'a> {
    name: &'a str,
    field: &'a FieldFile,
    script: &'a FieldScript<'a>,
    inits: Vec<EntityInit>,
    mesh: Walkmesh,
    cast: Vec<CastMember>,
    /// The player-bound entities that `GETAI` reads (else every player-bound entity).
    seed_entities: Vec<u8>,
    director: usize,
    /// Every persistent byte some script of the field writes (a word write counts both bytes).
    written: BTreeSet<(u8, u8)>,
}

fn ename(info: &Info, entity: usize) -> String {
    match info.script.entity_name(entity) {
        Some(n) if !n.is_empty() => n.to_string(),
        _ => format!("e{entity}"),
    }
}

/// `eN:name` for headers.
fn elabel(info: &Info, entity: usize) -> String {
    match info.script.entity_name(entity) {
        Some(n) if !n.is_empty() => format!("e{entity}:{n}"),
        _ => format!("e{entity}"),
    }
}

/// The distinct scripts of an entity: the lowest slot and all the slots that share the entry point, by offset.
fn distinct_scripts(script: &FieldScript, entity: usize) -> Vec<(usize, Vec<usize>)> {
    let mut by_start: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for slot in 0..SCRIPTS_PER_ENTITY {
        if let Some(span) = script.span(entity, slot) {
            by_start.entry(span.start).or_default().push(slot);
        }
    }
    by_start.into_values().filter_map(|slots| Some((*slots.first()?, slots))).collect()
}

/// `(bank, address)` an instruction writes, for the opcodes that write a variable.
fn write_target(ins: &Instruction) -> Option<(u8, u8)> {
    if !matches!(ins.opcode, 0x76..=0x7D | 0x80..=0x9C) {
        return None;
    }
    let o = ins.operand("var")?;
    (o.bank != 0).then(|| (o.bank, u8::try_from(o.value & 0xFF).unwrap_or(0)))
}

/// The bytes an instruction writes: its variable's byte, the next one too for a word, `HBYTE`'s high byte, the byte a bit
/// of a row of bits lives in.
fn written_bytes(ins: &Instruction) -> Vec<(u8, u8)> {
    let Some((bank, address)) = write_target(ins) else { return Vec::new() };
    let next = address.checked_add(1).map(|a| (bank, a));
    match ins.opcode {
        // words: PLUS2!, MINUS2!, INC2!, DEC2!, SETWORD, the word arithmetic of 0x86..=0x98 (the even opcodes), 2BYTE
        0x77 | 0x79 | 0x7B | 0x7D | 0x81 | 0x86 | 0x88 | 0x8A | 0x8C | 0x8E | 0x90 | 0x92 | 0x94 | 0x96 | 0x98 | 0x9C => [Some((bank, address)), next].into_iter().flatten().collect(),
        0x9B => next.into_iter().collect(),
        0x82..=0x84 => {
            let row = ins.literal("bit").and_then(|b| u8::try_from(b / 8).ok()).and_then(|b| address.checked_add(b));
            vec![(bank, row.unwrap_or(address))]
        }
        _ => vec![(bank, address)],
    }
}

fn scan_written(script: &FieldScript) -> BTreeSet<(u8, u8)> {
    let mut out = BTreeSet::new();
    for entity in 0..script.entity_count() {
        for (slot, _) in distinct_scripts(script, entity) {
            if let Ok(d) = script.disassemble_partial(entity, slot) {
                out.extend(d.instructions.iter().flat_map(written_bytes));
            }
        }
    }
    out
}

/// What the script that holds `site` requests itself, in order (`REQ`/`REQSW`/`REQEW`, literal targets).
fn host_requests(info: &Info, site: &Site) -> Vec<String> {
    let Ok(d) = info.script.disassemble_partial(site.entity, site.slot) else { return Vec::new() };
    d.instructions
        .iter()
        .filter(|i| matches!(i.opcode, 0x01..=0x03))
        .filter_map(|i| {
            let (e, s) = (usize::try_from(i.literal("entity")?).ok()?, i.literal("script")?);
            Some(format!("{}.s{s}({})", ename(info, e), i.mnemonic))
        })
        .collect()
}

fn in_main_part(info: &Info, entity: usize, slot: usize, offset: usize) -> bool {
    slot == 0 && info.inits.get(entity).and_then(|i| i.main_start).is_some_and(|m| offset >= m)
}

fn scan_sites(info: &Info) -> Vec<Site> {
    let mut out = Vec::new();
    for entity in 0..info.script.entity_count() {
        for (slot, shared) in distinct_scripts(info.script, entity) {
            let Ok(d) = info.script.disassemble_partial(entity, slot) else { continue };
            for ins in &d.instructions {
                let Some((bank, address)) = write_target(ins) else { continue };
                let what = match (bank, address) {
                    (2, 0) | (2, 1) => What::Moment { value: matches!(ins.opcode, 0x80 | 0x81).then(|| ins.literal("value")).flatten(), high: address == 1 },
                    (1, 225) if matches!(ins.opcode, 0x82..=0x84) => What::Bit { op: ins.mnemonic, bit: ins.literal("bit") },
                    (1, 225) => What::Byte,
                    _ => continue,
                };
                out.push(Site { entity, slot, shared: shared.len(), offset: ins.offset, text: ins.to_string(), what, in_main: in_main_part(info, entity, slot, ins.offset) });
            }
        }
    }
    out
}

/// Every script that requests script `slot` of `entity` (by entry point: slots that share one are the same script).
fn scan_requesters(info: &Info, site: &Site) -> Vec<Requester> {
    let Some(target) = info.script.span(site.entity, site.slot) else { return Vec::new() };
    let mut out = Vec::new();
    for entity in 0..info.script.entity_count() {
        for (slot, _) in distinct_scripts(info.script, entity) {
            let Ok(d) = info.script.disassemble_partial(entity, slot) else { continue };
            for ins in d.instructions.iter().filter(|i| matches!(i.opcode, 0x01..=0x03)) {
                let (Some(e), Some(s)) = (ins.literal("entity"), ins.literal("script")) else { continue };
                let (Ok(e), Ok(s)) = (usize::try_from(e), usize::try_from(s)) else { continue };
                if e == site.entity && info.script.span(e, s).map(|sp| sp.start) == Some(target.start) {
                    out.push(Requester { entity, slot, offset: ins.offset, op: ins.mnemonic, in_main: in_main_part(info, entity, slot, ins.offset) });
                }
            }
        }
    }
    out
}

/// `(reader entity, slot, offset, entity read)` of every `GETAI` of the field.
fn scan_getai(info: &Info) -> Vec<(usize, usize, usize, usize)> {
    let mut out = Vec::new();
    for entity in 0..info.script.entity_count() {
        for (slot, _) in distinct_scripts(info.script, entity) {
            let Ok(d) = info.script.disassemble_partial(entity, slot) else { continue };
            for ins in d.instructions.iter().filter(|i| i.opcode == 0xB9) {
                if let Some(target) = ins.literal("entity").and_then(|e| usize::try_from(e).ok()) {
                    out.push((entity, slot, ins.offset, target));
                }
            }
        }
    }
    out
}

/// What the stage setup is supposed to run: script 0 of an entity up to its first `RET`. Prints the static facts and
/// returns how many instructions the init parts hold in all.
fn check_init_parts(info: &Info) -> usize {
    let mut total = 0usize;
    let mut notes = Vec::new();
    for entity in 0..info.script.entity_count() {
        let Ok(d) = info.script.disassemble_partial(entity, 0) else { continue };
        let Some(ret) = d.instructions.iter().position(|i| i.opcode == 0x00) else {
            notes.push(format!("{} has no RET in script 0 (it runs on into the next script)", elabel(info, entity)));
            total += d.instructions.len();
            continue;
        };
        let (ret_offset, init) = (d.instructions.get(ret).map_or(0, |i| i.offset), d.instructions.get(..=ret).unwrap_or(&[]));
        total += init.len();
        for ins in init {
            match ins.jump_target {
                Some(t) if t > ret_offset => notes.push(format!("{}: init jump @{:04X} goes past its RET (to {t:04X})", elabel(info, entity), ins.offset)),
                Some(t) if t <= ins.offset => notes.push(format!("{}: init loop @{:04X} jumps back to {t:04X}", elabel(info, entity), ins.offset)),
                _ => {}
            }
        }
    }
    if notes.is_empty() {
        println!("    init parts: {total} instructions in all; none jumps past its RET or loops");
    } else {
        for n in &notes {
            println!("    init parts: !! {n}");
        }
    }
    total
}

// ---- building a scene --------------------------------------------------------------------------------------------

fn build_cast(script: &FieldScript, inits: &[EntityInit], loader: &ModelLoader) -> (Vec<CastMember>, Vec<String>) {
    let mut cast = Vec::new();
    let mut notes = Vec::new();
    for init in inits.iter().filter(|i| i.is_character()) {
        let Some(model) = init.model_index.and_then(|i| loader.models.get(i)) else {
            notes.push(format!("entity {} has CHAR but the loader has no record {:?}", init.entity, init.model_index));
            continue;
        };
        if init.model_ref.as_ref().is_some_and(|r| r.hrc != model.hrc) {
            notes.push(format!("entity {}: parse_model_list names another skeleton than the loader", init.entity));
        }
        let Ok(entity) = u8::try_from(init.entity) else { continue };
        let idle = model.animations.first().map(|a| a.stem()).unwrap_or_default();
        if idle.is_empty() {
            notes.push(format!("entity {} ({}): the loader names no animation", init.entity, script.entity_name(init.entity).unwrap_or("")));
        }
        cast.push(CastMember { entity, name: init.name.clone(), look: Look::Model { hrc: model.hrc_stem(), idle }, start: None });
    }
    (cast, notes)
}

/// The centre of triangle `t`, on the triangle.
fn centroid(mesh: &Walkmesh, t: u16) -> Option<(i16, i16)> {
    let tri = mesh.triangles.get(usize::from(t))?;
    let (sx, sy): (i32, i32) = tri.iter().fold((0, 0), |(x, y), v| (x + i32::from(v.x), y + i32::from(v.y)));
    let (x, y) = (sx.div_euclid(3), sy.div_euclid(3));
    if triangle_contains_xy(tri, x, y) {
        Some((i16::try_from(x).ok()?, i16::try_from(y).ok()?))
    } else {
        None
    }
}

fn host_step(site: &Site) -> Option<Step> {
    let entity = u8::try_from(site.entity).ok()?;
    Some(if site.in_main { Step::StartMain(entity) } else { Step::Await(ScriptRef::new(entity, u8::try_from(site.slot).ok()?)) })
}

/// The main part of every entity (the director first). When the write is not in a main part the host script is STARTED
/// first (not awaited): it and the main part of its own entity share one priority slot in the interpreter (every step of a
/// scene's root runs at priority 7), so a main started before it would hold the slot for ever and the host would never
/// run. Started first, the host takes the slot and its entity's main waits behind it.
fn all_mains_root(info: &Info, site: &Site) -> Vec<Step> {
    let mut root: Vec<Step> = Vec::new();
    if !site.in_main {
        if let (Ok(entity), Ok(slot)) = (u8::try_from(site.entity), u8::try_from(site.slot)) {
            root.push(Step::Start(ScriptRef::new(entity, slot)));
        }
    }
    let mut order: Vec<usize> = vec![info.director];
    order.extend((0..info.script.entity_count()).filter(|&e| e != info.director));
    root.extend(order.into_iter().filter(|&e| info.inits.get(e).is_some_and(|i| i.main_start.is_some())).filter_map(|e| u8::try_from(e).ok().map(Step::StartMain)));
    root
}

fn step_name(info: &Info, step: &Step) -> String {
    match step {
        Step::StartMain(e) => format!("StartMain({})", ename(info, usize::from(*e))),
        Step::Await(r) => format!("Await({}.s{})", ename(info, usize::from(r.entity)), r.slot),
        Step::Start(r) => format!("Start({}.s{})", ename(info, usize::from(r.entity)), r.slot),
        Step::SetBit { bank, address, bit } => format!("SetBit({bank},{address},{bit})"),
    }
}

struct Variant {
    name: String,
    root: Vec<Step>,
    /// Variables the run is told it was found with (only the `probe` variant has any).
    given: Vec<Given>,
}

/// The variable the host script compares with the literal `triangle` (`IFSW/IFUW/IFUB Var[b][a] == triangle`): the one a
/// director's `GETAI` loop fills for a script that only waits for the player to stand there (the elevator's panel).
fn triangle_var(info: &Info, site: &Site, triangle: u16) -> Option<(u8, u8)> {
    let d = info.script.disassemble_partial(site.entity, site.slot).ok()?;
    d.instructions.iter().filter(|i| matches!(i.opcode, 0x14 | 0x16 | 0x18) && i.literal("op") == Some(0)).find_map(|i| {
        let (left, right) = (i.operand("left")?, i.operand("right")?);
        (left.bank != 0 && right.literal() == Some(i32::from(triangle))).then(|| (left.bank, u8::try_from(left.value & 0xFF).unwrap_or(0)))
    })
}

fn variants(info: &Info, site: &Site, requesters: &[Requester], triangle: Option<u16>) -> Vec<Variant> {
    let mut out = Vec::new();
    if let Some(step) = host_step(site) {
        out.push(Variant { name: format!("host {}", step_name(info, &step)), root: vec![step], given: Vec::new() });
        // A main part that waits on a variable another entity's loop fills with the player's triangle: a PROBE (not a
        // run of the field) that is told the variable already holds it, to see what the script does next.
        if let (true, Some(t)) = (site.in_main, triangle) {
            if let Some((bank, address)) = triangle_var(info, site, t) {
                let value = u8::try_from(t).unwrap_or(u8::MAX);
                out.push(Variant { name: format!("probe host, Var[{bank}][{address}]={t} given"), root: vec![step], given: vec![Given { bank, address, value }] });
            }
        }
    }
    out.push(Variant { name: "all-mains".to_string(), root: all_mains_root(info, site), given: Vec::new() });
    let mut seen = BTreeSet::new();
    for r in requesters.iter().filter(|r| r.slot >= 1) {
        if let (Ok(entity), Ok(slot)) = (u8::try_from(r.entity), u8::try_from(r.slot)) {
            if seen.insert((entity, slot)) {
                let step = Step::Await(ScriptRef::new(entity, slot));
                out.push(Variant { name: format!("via {}", step_name(info, &step)), root: vec![step], given: Vec::new() });
            }
        }
    }
    out
}

/// Where the player's entities stand in a run that has a triangle to stand on.
#[derive(Clone, Copy)]
struct Seed {
    triangle: u16,
    x: i16,
    y: i16,
}

fn build_def(info: &Info, site: &Site, root: Vec<Step>, given: &[Given], moment: u16, seed: Option<Seed>) -> SceneDef {
    let mut cast = info.cast.clone();
    if let Some(s) = seed {
        for member in cast.iter_mut().filter(|m| info.seed_entities.contains(&m.entity)) {
            member.start = Some(Spot { x: s.x, y: s.y, triangle: s.triangle, direction: 0 });
        }
    }
    SceneDef {
        id: format!("ff7/{}/point-survey", info.name),
        game: "ff7".to_string(),
        field: info.name.to_string(),
        trigger: Trigger { entity: u8::try_from(site.entity).unwrap_or(0), name: ename(info, site.entity), slot: u8::try_from(site.slot).unwrap_or(0), talk_radius: 0, moments: moment..=moment },
        setup: (0..info.script.entity_count()).filter_map(|e| u8::try_from(e).ok()).map(|e| ScriptRef::new(e, 0)).collect(),
        given: given.to_vec(),
        root,
        cast,
        speakers: Vec::new(),
        witness: Vec::new(),
        arrival: Spot { x: 0, y: 0, triangle: 0, direction: 0 },
        presents: None, resume_after_battle: None,
    }
}

// ---- running a scene ---------------------------------------------------------------------------------------------

/// One window of a run: who opened it, which dialog (none: the interpreter's own note), and the lines it asks.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Win {
    opener: u8,
    dialog: Option<usize>,
    ask: Option<(usize, usize)>,
}

/// What driving a `ScenePlay` shows (the `Outcome` of `run_to_end` has the rest).
struct Driven {
    end: End,
    wins: Vec<Win>,
    /// `(first, last, cursor)` of every asking window, in order.
    asks: Vec<(usize, usize, usize)>,
    /// Stage events of the scene by entity (`None`: the camera), without the initial placements.
    stage: BTreeMap<Option<u8>, u32>,
    /// `Place` and `Move` events by entity.
    moves: BTreeMap<u8, (u32, u32)>,
    off_cast: u32,
    /// Where the stage left each cast entity before the scene began: `(x, y, triangle)`.
    initial: BTreeMap<u8, (i16, i16, Option<u16>)>,
    runaway: bool,
}

fn stage_entity(stage: &Stage) -> Option<u8> {
    match stage {
        Stage::Place { entity, .. } | Stage::Move { entity, .. } | Stage::Face { entity, .. } | Stage::Anim { entity, .. } | Stage::Show { entity, .. } => Some(*entity),
        Stage::Camera(_) => None,
    }
}

fn drive(field: &FieldFile, def: &SceneDef, story: &StoryState, answers: &[usize]) -> Result<Driven, String> {
    let mut play = ScenePlay::new(field, def, story, SEED).map_err(|e| e.to_string())?;
    let mut initial = BTreeMap::new();
    for s in play.initial_stage_events() {
        if let Stage::Place { entity, x, y, triangle, .. } = s {
            initial.insert(*entity, (*x, *y, *triangle));
        }
    }
    let mut answers = answers.iter().copied();
    let mut given: Option<usize> = None;
    let (mut wins, mut asks, mut stage, mut moves) = (Vec::new(), Vec::new(), BTreeMap::new(), BTreeMap::<u8, (u32, u32)>::new());
    let mut beats = 0usize;
    let mut runaway = false;
    let end = loop {
        let beat = play.next(given.take());
        for t in &beat.stage {
            *stage.entry(stage_entity(&t.what)).or_insert(0u32) += 1;
            match &t.what {
                Stage::Place { entity, .. } => moves.entry(*entity).or_default().0 += 1,
                Stage::Move { entity, .. } => moves.entry(*entity).or_default().1 += 1,
                _ => {}
            }
        }
        match beat.then {
            Then::End { how, .. } => break how,
            Then::Window(w) => {
                let dialog = match w.message.origin {
                    MessageOrigin::Canon { dialog, .. } => Some(dialog),
                    MessageOrigin::Added { .. } => None,
                };
                let ask = w.message.choice.map(|c| (c.first, c.last));
                if let Some(c) = w.message.choice {
                    asks.push((c.first, c.last, c.cursor));
                    given = answers.next();
                }
                wins.push(Win { opener: w.opener, dialog, ask });
            }
        }
        beats += 1;
        if beats > 2000 {
            runaway = true;
            break End::Budget;
        }
    };
    Ok(Driven { end, wins, asks, stage, moves, off_cast: play.off_cast(), initial, runaway })
}

struct Run {
    out: Outcome,
    drv: Driven,
    notes: Vec<String>,
}

fn run_once(info: &Info, def: &SceneDef, moment: u16, answers: &[usize]) -> Result<Run, String> {
    let story = StoryState::at(moment);
    let out = run_to_end(info.field, def, &story, SEED, answers).map_err(|e| e.to_string())?;
    let drv = drive(info.field, def, &story, answers)?;
    let mut notes = Vec::new();
    if drv.end != out.end || drv.wins.len() != out.windows {
        notes.push(format!("driver and run_to_end disagree (end {:?} vs {:?}, windows {} vs {})", drv.end, out.end, drv.wins.len(), out.windows));
    }
    if drv.runaway {
        notes.push("driver stopped a run of more than 2000 beats".to_string());
    }
    Ok(Run { out, drv, notes })
}

/// What a run did, as one string: two runs with the same one are the same run (the counters that only differ by how long
/// a run that never parks went on are left out).
fn signature(r: &Run) -> String {
    format!(
        "{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{}|{}|{:?}",
        r.out.end,
        r.drv.wins,
        r.out.writes,
        r.out.jump,
        r.drv.stage,
        r.out.stats.unknown,
        r.out.stats.unresolved > 0,
        r.out.stats.dropped_requests,
        r.out.unknown_persistent_reads
    )
}

/// The runs of one case, each with the answers it was given, and whether the combinations ran out before the cap.
type Explored = (Vec<(Vec<usize>, Run)>, bool);

/// Run a case for every combination of answers its asking windows allow (the first run takes the cursor's lines).
fn explore(info: &Info, def: &SceneDef, moment: u16) -> Result<Explored, String> {
    let mut done: Vec<(Vec<usize>, Run)> = Vec::new();
    let mut seen = BTreeSet::new();
    let mut stack: Vec<Vec<usize>> = vec![Vec::new()];
    let (mut runs, mut capped) = (0usize, false);
    while let Some(prefix) = stack.pop() {
        if runs >= MAX_ANSWER_RUNS {
            capped = true;
            break;
        }
        runs += 1;
        let run = run_once(info, def, moment, &prefix)?;
        if let Some(&(first, last, _)) = run.drv.asks.get(prefix.len()) {
            for choice in (first..=last).rev() {
                let mut next = prefix.clone();
                next.push(choice);
                stack.push(next);
            }
        }
        if seen.insert(signature(&run)) {
            done.push((prefix, run));
        }
    }
    Ok((done, capped))
}

// ---- verdicts and printing ---------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Verdict {
    Yes,
    /// It happened, but the run executed a `GETAI` and read something it could not resolve.
    Unverified,
    /// The stage setup made the write (no window can open there).
    Setup,
    No,
}

fn has_expected(w: &Writes, e: Expect) -> bool {
    match e {
        Expect::Moment(v) => w.moment == Some(v),
        Expect::Bit(b) => w.tracked.iter().any(|&((bank, address), value)| (bank, address) == (1, 225) && value & (1u8 << b) != 0),
    }
}

fn judge(e: Expect, out: &Outcome, baseline: &Outcome) -> Verdict {
    if !has_expected(&out.writes, e) {
        Verdict::No
    } else if has_expected(&baseline.writes, e) {
        Verdict::Setup
    } else if out.stats.unresolved > 0 && out.stats.handled.contains_key("GETAI") {
        // Only an unresolved GETAI sets a variable wrongly (it leaves 0, which another unset variable equals). The other
        // unresolved reads (a turn or a walk towards an entity the scene has not placed) change no variable.
        Verdict::Unverified
    } else {
        Verdict::Yes
    }
}

fn verdict_text(v: Verdict) -> &'static str {
    match v {
        Verdict::Yes => "YES",
        Verdict::Unverified => "YES?",
        Verdict::Setup => "SETUP",
        Verdict::No => "no",
    }
}

fn expect_text(expect: &[Expect], out: &Outcome, baseline: &Outcome) -> String {
    expect
        .iter()
        .map(|&e| {
            let name = match e {
                Expect::Moment(v) => format!("m={v}"),
                Expect::Bit(b) => format!("b{b}"),
            };
            format!("{name}:{}", verdict_text(judge(e, out, baseline)))
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn counts(map: &BTreeMap<&'static str, u32>, keep: impl Fn(&str) -> bool) -> String {
    let parts: Vec<String> = map.iter().filter(|(k, _)| keep(k)).map(|(k, n)| format!("{k}:{n}")).collect();
    if parts.is_empty() {
        "-".to_string()
    } else {
        parts.join(",")
    }
}

/// The listed opcodes the interpreter skipped (or, for `BATTLE`, handled), whichever table they ended up in.
fn listed(s: &Stats) -> String {
    let parts: Vec<String> = LISTED
        .iter()
        .filter_map(|&name| {
            if let Some(n) = s.ignored.get(name) {
                Some(format!("{name}:{n}"))
            } else if let Some(n) = s.handled.get(name) {
                Some(format!("{name}(handled):{n}"))
            } else {
                s.unknown.get(name).map(|n| format!("{name}(unknown):{n}"))
            }
        })
        .collect();
    if parts.is_empty() {
        "-".to_string()
    } else {
        parts.join(",")
    }
}

fn stats_line(s: &Stats, verbose: bool) -> String {
    let mut line = format!(
        "unk{{{}}} listed{{{}}} also-skipped{{{}}} unres={} drop={} parked={} queued={} tails={} getai={} key-tests{{{}}}",
        counts(&s.unknown, |_| true),
        listed(s),
        counts(&s.ignored, |k| ALSO_SKIPPED.contains(&k)),
        s.unresolved,
        s.dropped_requests,
        s.parked,
        s.queued,
        s.quiescent_tails,
        s.handled.get("GETAI").copied().unwrap_or(0),
        counts(&s.handled, |k| matches!(k, "IFKEY" | "IFKEYON" | "IFKEYOFF"))
    );
    if verbose {
        line.push_str(&format!(" other-ignored{{{}}}", counts(&s.ignored, |k| !LISTED.contains(&k) && !ALSO_SKIPPED.contains(&k))));
    }
    line
}

fn writes_line(w: &Writes, verbose: bool) -> String {
    let tracked: Vec<String> = w.tracked.iter().map(|&((b, a), v)| format!("({b},{a})={v:#04x}")).collect();
    let (mut persistent, mut scratch) = (Vec::new(), Vec::new());
    for (&(bank, address), &value) in &w.untracked {
        if (1..=4).contains(&bank) {
            persistent.push(format!("({bank},{address})={value}"));
        } else {
            scratch.push(format!("({bank},{address})={value}"));
        }
    }
    let scratch_text = if verbose { scratch.join(" ") } else { scratch.len().to_string() };
    format!("moment={:?} tracked=[{}] persistent=[{}] scratch={}", w.moment, tracked.join(" "), persistent.join(" "), scratch_text)
}

fn jump_text(j: Option<SceneJump>) -> String {
    j.map_or("-".to_string(), |j| format!("field {} at ({},{}) triangle {} dir {}", j.field, j.x, j.y, j.triangle, j.direction))
}

fn win_list(info: &Info, wins: &[Win]) -> String {
    if wins.is_empty() {
        return "-".to_string();
    }
    wins.iter()
        .map(|w| {
            let who = ename(info, usize::from(w.opener));
            match (w.dialog, w.ask) {
                (Some(d), Some((a, b))) => format!("{who}#{d}?[{a}-{b}]"),
                (Some(d), None) => format!("{who}#{d}"),
                (None, _) => format!("note@{who}"),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The unknown persistent reads, a `*` after the ones some script of the field also writes: the interpreter reads a byte
/// before it writes it (to see whether the write changes anything), so a variable the run only SETS is listed too.
fn upr_text(info: &Info, reads: &BTreeSet<(u8, u8)>) -> String {
    let parts: Vec<String> = reads.iter().map(|&(b, a)| format!("({b},{a}){}", if info.written.contains(&(b, a)) { "*" } else { "" })).collect();
    format!("[{}]", parts.join(" "))
}

fn stage_line(info: &Info, d: &Driven) -> String {
    let mut parts: Vec<String> = d
        .stage
        .iter()
        .map(|(k, n)| match k {
            Some(e) => format!("{}={n}", ename(info, usize::from(*e))),
            None => format!("camera={n}"),
        })
        .collect();
    parts.push(format!("off_cast={}", d.off_cast));
    parts.join(" ")
}

fn end_text(e: End) -> String {
    match e {
        End::MapJump { field } => format!("MapJump({field})"),
        other => format!("{other:?}"),
    }
}

/// One line of the closing table.
struct Row {
    field: String,
    point: String,
    site: String,
    variant: String,
    moments: String,
    seed: &'static str,
    end: String,
    windows: usize,
    exp: String,
    unres: u32,
    unknown: String,
    parked: u32,
    notes: String,
    /// Windows of the run that ask a question.
    asks: usize,
}

fn site_text(info: &Info, site: &Site) -> String {
    format!("{}.s{}{}@{:04X}", ename(info, site.entity), site.slot, if site.in_main { "m" } else { "" }, site.offset)
}

fn describe_site(info: &Info, site: &Site, reqs: &[Requester]) -> String {
    let what = match &site.what {
        What::Moment { .. } => "moment".to_string(),
        What::Bit { op, bit } => format!("{op} Var[1][225] bit {}", bit.map_or("?".to_string(), |b| b.to_string())),
        What::Byte => "Var[1][225] byte".to_string(),
    };
    let req: Vec<String> = reqs
        .iter()
        .map(|r| format!("{}.s{}{}(@{:04X} {})", ename(info, r.entity), r.slot, if r.in_main { "m" } else { "" }, r.offset, r.op))
        .collect();
    format!(
        "{} {}: {}{} requested by [{}]",
        site_text(info, site),
        what,
        site.text.split_once(": ").map_or(site.text.as_str(), |(_, t)| t),
        if site.shared > 1 { format!(" (+{} slots share it)", site.shared - 1) } else { String::new() },
        req.join(" ")
    )
}

// ---- one field ---------------------------------------------------------------------------------------------------

/// The writes the stage setup alone makes at `moment` (an empty root: no thread of the scene runs), kept by moment.
fn baseline(info: &Info, site: &Site, moment: u16, cache: &mut BTreeMap<u16, Outcome>) -> Result<Outcome, String> {
    if let Some(b) = cache.get(&moment) {
        return Ok(b.clone());
    }
    let def = build_def(info, site, Vec::new(), &[], moment, None);
    let b = run_to_end(info.field, &def, &StoryState::at(moment), SEED, &[]).map_err(|e| e.to_string())?;
    cache.insert(moment, b.clone());
    Ok(b)
}

/// Why a run of the main parts never settled: run it again without one main part at a time, and without every main part
/// that is neither the director's, the host's nor a cast member's (the lamps, the timers, the effects).
fn budget_probe(info: &Info, site: &Site, variant: &Variant, seed: Option<Seed>, moment: u16, answers: &[usize]) -> String {
    let ends = |root: Vec<Step>| -> Option<End> {
        let def = build_def(info, site, root, &variant.given, moment, seed);
        run_to_end(info.field, &def, &StoryState::at(moment), SEED, answers).ok().map(|o| o.end)
    };
    let mut single = Vec::new();
    for (i, step) in variant.root.iter().enumerate() {
        let Step::StartMain(e) = step else { continue };
        let mut root = variant.root.clone();
        root.remove(i);
        if ends(root).is_some_and(|end| end != End::Budget) {
            single.push(ename(info, usize::from(*e)));
        }
    }
    let cast: BTreeSet<u8> = info.cast.iter().map(|m| m.entity).collect();
    let host_main = if site.in_main { u8::try_from(site.entity).ok() } else { None };
    let director = u8::try_from(info.director).ok();
    let kept: Vec<Step> = variant
        .root
        .iter()
        .copied()
        .filter(|s| match s {
            Step::StartMain(e) => cast.contains(e) || Some(*e) == host_main || Some(*e) == director,
            _ => true,
        })
        .collect();
    let extras: Vec<String> = variant
        .root
        .iter()
        .filter_map(|s| match s {
            Step::StartMain(e) if !kept.contains(s) => Some(ename(info, usize::from(*e))),
            _ => None,
        })
        .collect();
    let without = ends(kept).map_or("?".to_string(), end_text);
    let alone = if single.is_empty() { "no single main part keeps it running".to_string() } else { format!("it stops being Budget without the main of [{}]", single.join(" ")) };
    format!("Budget: {alone}; without every non-cast non-director main [{}] it ends {without}", extras.join(" "))
}

/// One distinct result of a case, with the story moments that gave it.
struct Case {
    moments: Vec<u16>,
    answers: Vec<usize>,
    sig: String,
    run: Run,
    baseline: Outcome,
    capped: bool,
    /// For a run that ended in `Budget` with every main part started: [`budget_probe`].
    probe: Option<String>,
}

/// Run one variant (with or without the seed) at every moment, and fold the moments that give the very same run.
fn run_case(info: &Info, site: &Site, variant: &Variant, seed: Option<Seed>, moments: &[u16], cache: &mut BTreeMap<u16, Outcome>) -> Vec<Case> {
    let mut cases: Vec<Case> = Vec::new();
    for &moment in moments {
        let def = build_def(info, site, variant.root.clone(), &variant.given, moment, seed);
        let base = match baseline(info, site, moment, cache) {
            Ok(b) => b,
            Err(e) => {
                println!("      [{}] m={moment}: ERROR in the setup run: {e}", variant.name);
                continue;
            }
        };
        let (results, capped) = match explore(info, &def, moment) {
            Ok(r) => r,
            Err(e) => {
                println!("      [{}] m={moment}: ERROR {e}", variant.name);
                continue;
            }
        };
        for (answers, run) in results {
            let sig = format!("{}|{:?}|{}|{}", signature(&run), answers, run.out.setup_stats.setup_incomplete, counts(&run.out.setup_stats.unknown, |_| true));
            if let Some(case) = cases.iter_mut().find(|c| c.sig == sig) {
                case.moments.push(moment);
            } else {
                let probe = (run.out.end == End::Budget && variant.root.iter().filter(|s| matches!(s, Step::StartMain(_))).count() > 1).then(|| budget_probe(info, site, variant, seed, moment, &answers));
                cases.push(Case { moments: vec![moment], answers, sig, run, baseline: base.clone(), capped, probe });
            }
        }
    }
    cases
}

fn survey_field(lgp: &mut LgpArchive<std::fs::File>, name: &str, opts: &Opts, rows: &mut Vec<Row>) -> Result<(), String> {
    let field = load_field(lgp, name)?;
    let script = FieldScript::from_field(&field).map_err(|e| e.to_string())?;
    let models = field.section(section::MODEL_LOADER).ok().and_then(|s| parse_model_list(s).ok());
    let inits = script.entity_inits(models.as_ref());
    let loader = field.model_loader().map_err(|e| e.to_string())?;
    let mesh = field.walkmesh().map_err(|e| e.to_string())?;
    let (cast, cast_notes) = build_cast(&script, &inits, &loader);
    let director = inits.iter().position(|i| i.name.eq_ignore_ascii_case("dir")).unwrap_or(0);
    let written = scan_written(&script);
    let mut info = Info { name, field: &field, script: &script, inits, mesh, cast, seed_entities: Vec::new(), director, written };

    // The player's entities, and which of them the field's loops read the triangle of.
    let players: Vec<u8> = info.inits.iter().filter(|i| i.is_player()).filter_map(|i| u8::try_from(i.entity).ok()).collect();
    let getai = scan_getai(&info);
    let read: Vec<u8> = players.iter().copied().filter(|p| getai.iter().any(|g| g.3 == usize::from(*p))).collect();
    info.seed_entities = if read.is_empty() { players.clone() } else { read };
    let info = info;

    let labels = |entities: &[u8]| entities.iter().map(|&e| elabel(&info, usize::from(e))).collect::<Vec<_>>().join(" ");
    let cast_entities: Vec<u8> = info.cast.iter().map(|m| m.entity).collect();
    println!(
        "=== {name}: {} entities, cast (CHAR) [{}], players [{}] (seeded: [{}]), director {}, scale {}",
        script.entity_count(),
        labels(&cast_entities),
        labels(&players),
        labels(&info.seed_entities),
        elabel(&info, info.director),
        script.header().scale
    );
    for n in &cast_notes {
        println!("    cast: !! {n}");
    }
    let getai_text: Vec<String> = getai.iter().map(|g| format!("{}.s{}@{:04X} reads {}", ename(&info, g.0), g.1, g.2, elabel(&info, g.3))).collect();
    println!("    GETAI: {}", if getai_text.is_empty() { "none".to_string() } else { getai_text.join("; ") });
    let init_total = check_init_parts(&info);

    let sites = scan_sites(&info);
    let specs: Vec<&Spec> = SPECS.iter().filter(|s| s.field == name).collect();
    println!("    story writes found ({}):", sites.len());
    for site in &sites {
        let reqs = scan_requesters(&info, site);
        let mut covered: Vec<String> = specs.iter().filter(|s| locates(s, site)).map(|s| s.id.to_string()).collect();
        for spec in &specs {
            let located: Vec<&Site> = sites.iter().filter(|l| locates(spec, l)).collect();
            if expected_by(spec, site, &located) {
                covered.push(format!("{} (a bit it expects, same script)", spec.id));
            }
        }
        let verdict = if covered.is_empty() { "not in the point list (not run)".to_string() } else { format!("point {}", covered.join(",")) };
        println!("      {} -> {verdict}", describe_site(&info, site, &reqs));
    }
    if sites.is_empty() {
        println!("      none: this field has no story point");
    }

    let mut cache: BTreeMap<u16, Outcome> = BTreeMap::new();
    for spec in &specs {
        let located: Vec<&Site> = sites.iter().filter(|s| locates(spec, s)).collect();
        println!("  --- point {}  {}", spec.id, spec.label);
        if located.is_empty() {
            println!("      !! no matching write found in this field");
            continue;
        }
        let moments: Vec<u16> = opts.at.map_or_else(|| spec.moments.to_vec(), |m| vec![m]);
        let seed_spot = spec.triangle.and_then(|t| centroid(&info.mesh, t).map(|(x, y)| Seed { triangle: t, x, y }));
        if let (Some(t), None) = (spec.triangle, seed_spot) {
            println!("      !! triangle {t} has no centroid on the walkmesh: no seeded run");
        }
        let seeds: Vec<(Option<Seed>, &'static str)> = match seed_spot {
            Some(s) => vec![(Some(s), "seeded"), (None, "control")],
            None => vec![(None, "-")],
        };
        for site in located {
            let reqs = scan_requesters(&info, site);
            println!("      write {}", describe_site(&info, site, &reqs));
            let requests = host_requests(&info, site);
            println!("      the host script requests: {}", if requests.is_empty() { "nothing".to_string() } else { requests.join(" ") });
            for variant in variants(&info, site, &reqs, spec.triangle) {
                // (the probe is told the variable the director's loop would fill, so the seed has nothing to do there)
                let plain: [(Option<Seed>, &'static str); 1] = [(None, "-")];
                let variant_seeds: &[(Option<Seed>, &'static str)] = if variant.given.is_empty() { &seeds } else { &plain };
                let runs: Vec<(Option<Seed>, &'static str, Vec<Case>)> =
                    variant_seeds.iter().map(|&(seed, seed_name)| (seed, seed_name, run_case(&info, site, &variant, seed, &moments, &mut cache))).collect();
                // What the same runs without the seed wrote: a seeded run is only gated by the triangle if this says no.
                let control: Vec<String> = runs.iter().filter(|r| r.1 == "control").flat_map(|r| r.2.iter().map(|c| expect_text(spec.expect, &c.run.out, &c.baseline))).collect();
                let control = if control.is_empty() { None } else { Some(control.join(" | ")) };
                for (seed, seed_name, cases) in &runs {
                    for case in cases {
                        let ctl = if *seed_name == "seeded" { control.as_deref() } else { None };
                        print_case(&info, spec, site, &variant, *seed, seed_name, case, ctl, opts, init_total, rows);
                    }
                }
            }
        }
    }
    // Setup facts, once per field: how many instructions the stage setup executed against what the init parts hold.
    if let Some(b) = cache.iter().next() {
        let executed = exec_count(&b.1.setup_stats);
        println!("    setup at m={}: executed {executed} instructions ({init_total} in the init parts){}", b.0, if executed > init_total { "  !! more than the init parts hold: something ran past a RET or looped" } else { "" });
    }
    println!();
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn print_case(info: &Info, spec: &Spec, site: &Site, variant: &Variant, seed: Option<Seed>, seed_name: &'static str, case: &Case, control: Option<&str>, opts: &Opts, init_total: usize, rows: &mut Vec<Row>) {
    let (run, out) = (&case.run, &case.run.out);
    let exp = match control {
        Some(c) => format!("{} (control: {c})", expect_text(spec.expect, out, &case.baseline)),
        None => expect_text(spec.expect, out, &case.baseline),
    };
    let mut notes = run.notes.clone();
    if case.capped {
        notes.push(format!("answers: stopped at {MAX_ANSWER_RUNS} runs"));
    }
    if out.setup_stats.setup_incomplete {
        notes.push("setup incomplete".to_string());
    }
    if let Some(p) = &case.probe {
        notes.push(p.clone());
    }
    if let Some(s) = seed {
        for &e in &info.seed_entities {
            let at = run.drv.initial.get(&e);
            if at.is_none_or(|p| p.2 != Some(s.triangle)) {
                notes.push(format!("seed on {} not in force at the start (stage left it at {:?})", ename(info, usize::from(e)), at));
            }
            if let Some((places, moves)) = run.drv.moves.get(&e) {
                notes.push(format!("{} re-placed by the scene: {places} place + {moves} move events", ename(info, usize::from(e))));
            }
        }
    }
    let moments = case.moments.iter().map(u16::to_string).collect::<Vec<_>>().join("+");
    let answers = if case.answers.is_empty() { String::new() } else { format!(" answers={:?}", case.answers) };
    let seed_text = match (seed, seed_name) {
        (Some(s), _) => format!(" seeded (triangle {} at {},{})", s.triangle, s.x, s.y),
        (None, "control") => " CONTROL (no seed)".to_string(),
        _ => String::new(),
    };
    let header = format!("      [{}]{seed_text} m={moments}{answers} -> end={} windows={} expect {exp}", variant.name, end_text(out.end), out.windows);
    let control = seed_name == "control";
    if opts.brief || control {
        println!("{header}{}", if notes.is_empty() { String::new() } else { format!("  !! {}", notes.join("; ")) });
    } else {
        println!("{header}");
        println!("         windows: {}", win_list(info, &run.drv.wins));
        println!("         writes: {}   jump: {}", writes_line(&out.writes, opts.verbose), jump_text(out.jump));
        println!("         setup: {} incomplete={}", stats_line(&out.setup_stats, opts.verbose), out.setup_stats.setup_incomplete);
        println!("         scene: {} steps={} threads={}", stats_line(&out.stats, opts.verbose), out.stats.steps, out.stats.threads);
        println!("         unknown_persistent_reads: {} (* = a script of the field also writes it)", upr_text(info, &out.unknown_persistent_reads));
        println!("         stage: {}", stage_line(info, &run.drv));
        if opts.verbose {
            println!("         setup executed {} instructions ({init_total} in the init parts)", exec_count(&out.setup_stats));
        }
        if !notes.is_empty() {
            println!("         !! {}", notes.join("; "));
        }
    }
    let unknown: Vec<String> = out.stats.unknown.iter().map(|(k, n)| format!("{k}:{n}")).collect();
    rows.push(Row {
        field: info.name.to_string(),
        point: spec.id.to_string(),
        site: site_text(info, site),
        variant: variant.name.clone(),
        moments,
        seed: seed_name,
        end: end_text(out.end),
        windows: out.windows,
        exp,
        unres: out.stats.unresolved,
        unknown: if unknown.is_empty() { "-".to_string() } else { unknown.join(",") },
        parked: out.stats.parked,
        notes: notes.join("; "),
        asks: run.drv.asks.len(),
    });
}

fn locates(spec: &Spec, site: &Site) -> bool {
    match (spec.locate, &site.what) {
        (Locate::Moment(v), What::Moment { value: Some(w), high: false }) => v == *w,
        (Locate::Bit(b), What::Bit { op: "BITON", bit: Some(w) }) => b == *w,
        _ => false,
    }
}

/// A `BITON` of a bit the point expects is not run on its own: it is in the script of the point's run, which checks it.
fn expected_by(spec: &Spec, site: &Site, located: &[&Site]) -> bool {
    match &site.what {
        What::Bit { op: "BITON", bit: Some(b) } => {
            spec.expect.iter().any(|e| matches!(e, Expect::Bit(x) if i32::from(*x) == *b)) && located.iter().any(|l| l.entity == site.entity && l.slot == site.slot)
        }
        _ => false,
    }
}

/// Instructions a run's counters say it executed.
fn exec_count(s: &Stats) -> usize {
    s.handled.values().chain(s.ignored.values()).chain(s.unknown.values()).map(|&n| n as usize).sum()
}

fn print_table(rows: &[Row]) {
    println!("==== summary: one row per distinct run (m = story moments that gave this run; exp = expected write: YES, YES? = happened, but a GETAI ran with an unresolved read, SETUP = made by the stage setup, no = did not happen)");
    println!("{:<8} {:<11} {:<24} {:<26} {:<6} {:<8} {:<14} {:>3} {:<34} {:>5} {:>6} {:<12} notes", "field", "point", "site", "variant", "m", "seed", "end", "win", "exp", "unres", "parked", "unknown");
    for r in rows {
        println!("{:<8} {:<11} {:<24} {:<26} {:<6} {:<8} {:<14} {:>3} {:<34} {:>5} {:>6} {:<12} {}", r.field, r.point, r.site, r.variant, r.moments, r.seed, r.end, r.windows, r.exp, r.unres, r.parked, trim_unknown(&r.unknown), r.notes);
    }
    let asking = rows.iter().filter(|r| r.asks > 0).count();
    println!(
        "{} rows; {asking} of them have a window that asks (every answer of such a window is tried, up to {MAX_ANSWER_RUNS} runs a case; with none, no answers were needed)",
        rows.len()
    );
}

/// The unknown ops of a row without the two the stage setup always counts (`CHAR`, `PC`).
fn trim_unknown(unknown: &str) -> String {
    let kept: Vec<&str> = unknown.split(',').filter(|u| u.contains(':') && !BENIGN_UNKNOWN.iter().any(|b| u.starts_with(&format!("{b}:")))).collect();
    if kept.is_empty() {
        "-".to_string()
    } else {
        kept.join(",")
    }
}

fn main() {
    let Some(dir) = data_dir() else {
        eprintln!("FMV_FF7_DATA is not set; skipping (point it at the game's data/ directory)");
        return;
    };
    let mut opts = Opts::default();
    let mut names: Vec<String> = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--brief" => opts.brief = true,
            "--verbose" => opts.verbose = true,
            "--at" => opts.at = args.next().and_then(|v| v.parse().ok()),
            other => names.push(other.to_string()),
        }
    }
    let path = flevel_path(&dir);
    let mut lgp = match LgpArchive::open_path(&path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("cannot open {}: {e}", path.display());
            return;
        }
    };
    if names.is_empty() {
        names = FIELDS.iter().map(|s| (*s).to_string()).collect();
    }
    let mut rows = Vec::new();
    for name in &names {
        if let Err(e) = survey_field(&mut lgp, name, &opts, &mut rows) {
            eprintln!("{name}: {e}");
        }
    }
    print_table(&rows);
}
