//! Who reads and who writes the route's story flags: for each `bank:address` variable on the command line, every
//! field-script instruction that touches one of its bytes, the bits it touches, whether it reads, writes or does both,
//! and the story-moment tests (`Var[2][0]` against a literal) that guard it in its script. Then, for the variable, the
//! fields that read it and the fields that write it, and a verdict for the whole value and for each of its bits.
//!
//! Needs the owner's data (skips otherwise). Reads only, writes nothing. Prints field and entity names, numbers and
//! mnemonics, never game text. Only field scripts (`flevel.lgp`) are scanned, not the battle or world-map scripts.
//!
//! ```text
//! FMV_FF7_DATA=<.../data> cargo run -p fmv-ff7 --example var_readers -- 3:66 3:111 1:4
//! ```
//!
//! Kinds: `R` reads, `W` writes (BITON and BITOFF write one bit each), `RW` reads then writes (arithmetic, BITXOR), `?` a
//! direction the code does not settle (counted both ways). A guard is a forward jump before the access whose target lies beyond it (`guards_of`
//! in `story.rs`, minus the unconditional `JMPF`): the access runs only when that jump is not taken, so the comparison
//! printed is the one under which it runs. `(bypassable)` marks a guard that a jump from outside its stretch can skip
//! into. An access with no guard is `NOT under a moment test` (an `else` branch counts as unguarded).
#[path = "../tests/script_common.rs"]
mod common;

use common::{data_dir, flevel_path, is_field_name, load_field};
use fmv_ff7::lgp::LgpArchive;
use fmv_ff7::script::{FieldScript, Instruction, Operand, SCRIPTS_PER_ENTITY};
use std::collections::BTreeSet;

/// The story moment: the word at `Var[2][0]` (`story.rs`).
const MOMENT: (u8, usize) = (2, 0);

/// The `IF*` opcodes that compare two values (the jump is taken when the comparison is false).
const COMPARES: [&str; 6] = ["IFUB", "IFUBL", "IFSW", "IFSWL", "IFUW", "IFUWL"];

/// How an instruction uses the bytes of a variable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Read,
    Write,
    /// Read, then written (arithmetic, BITXOR).
    ReadWrite,
    /// A direction the code does not settle: counted as a read and as a write.
    Unknown,
}

impl Kind {
    fn reads(self) -> bool {
        matches!(self, Kind::Read | Kind::ReadWrite | Kind::Unknown)
    }
    fn writes(self) -> bool {
        matches!(self, Kind::Write | Kind::ReadWrite | Kind::Unknown)
    }
    /// Writes for certain (not `Unknown`).
    fn surely_writes(self) -> bool {
        matches!(self, Kind::Write | Kind::ReadWrite)
    }
    fn label(self) -> &'static str {
        match self {
            Kind::Read => "R",
            Kind::Write => "W",
            Kind::ReadWrite => "RW",
            Kind::Unknown => "?",
        }
    }
    fn merge(self, other: Kind) -> Kind {
        match (self, other) {
            (a, b) if a == b => a,
            (Kind::Unknown, _) | (_, Kind::Unknown) => Kind::Unknown,
            _ => Kind::ReadWrite,
        }
    }
}

use Kind::{ReadWrite as RW, Read as R, Unknown as U, Write as W};

/// `(operand name, what the opcode does with it, width in bytes)` for the banked operands of one opcode.
type Roles = &'static [(&'static str, Kind, usize)];

const ARITH_BYTE: Roles = &[("var", RW, 1), ("value", R, 1)];
const ARITH_WORD: Roles = &[("var", RW, 2), ("value", R, 2)];
/// Positions and triangles are read as words, though the layout gives their operands a byte each.
const POSITION_VARS: Roles = &[("var_x", W, 2), ("var_y", W, 2), ("var_z", W, 2), ("var_triangle", W, 2)];
const PALETTE: Roles = &[
    ("pos_src", R, 1),
    ("pos_dst", R, 1),
    ("start", R, 1),
    ("end", R, 1),
    ("b", R, 1),
    ("g", R, 1),
    ("r", R, 1),
    ("palette", R, 1),
    ("position", R, 1),
    ("tile_src", R, 1),
    ("tile_dst", R, 1),
];

/// The banked operands of an opcode (`key`: its mnemonic, or a `SPECIAL` sub-opcode's name) and what it does with them.
/// The widths follow the operand layout of `script/opcodes.rs` (`Bu8` one byte, `Bu16`/`Bs16` two, `Bs32` four), and the
/// writes and read-modify-writes follow the scene VM (`script/scene/vm.rs`) where it implements the opcode. Operands
/// whose direction the code does not settle are `Unknown`.
fn roles(key: &str) -> Roles {
    match key {
        "IFUB" | "IFUBL" => &[("left", R, 1), ("right", R, 1)],
        "IFSW" | "IFSWL" | "IFUW" | "IFUWL" => &[("left", R, 2), ("right", R, 2)],
        "PLUS" | "MINUS" | "MUL" | "DIV" | "MOD" | "AND" | "OR" | "XOR" | "INC" | "DEC" | "PLUS!" | "MINUS!" | "INC!" | "DEC!" => ARITH_BYTE,
        "PLUS2" | "MINUS2" | "MUL2" | "DIV2" | "MOD2" | "AND2" | "OR2" | "XOR2" | "INC2" | "DEC2" | "PLUS2!" | "MINUS2!" | "INC2!" | "DEC2!" => {
            ARITH_WORD
        }
        // BITON and BITOFF set one bit whatever it was (the other bits are kept): writes, not read-modify-writes
        "BITON" | "BITOFF" => &[("var", W, 1), ("bit", R, 1)],
        "BITXOR" => &[("var", RW, 1), ("bit", R, 1)],
        "SETBYTE" => &[("var", W, 1), ("value", R, 1)],
        "SETWORD" => &[("var", W, 2), ("value", R, 2)],
        "RANDOM" => &[("var", W, 1)],
        "LBYTE" => &[("var", W, 1), ("value", R, 1)],
        // HBYTE stores the high byte of the word at its address (see `byte_masks`)
        "HBYTE" => &[("var", W, 1), ("value", R, 2)],
        "2BYTE" => &[("var", W, 2), ("low", R, 1), ("high", R, 1)],
        "ASK" => &[("var", W, 1)],
        "GTPYE" => &[("var1", W, 1), ("var2", W, 1), ("var3", W, 1)],
        "SPTYE" => &[("character1", R, 1), ("character2", R, 1), ("character3", R, 1)],
        "GETPC" => &[("var_character", W, 1)],
        "PGTDR" | "GETDIR" => &[("var_direction", W, 1)],
        "PXYZI" | "AXYZI" => POSITION_VARS,
        "GETAXY" => &[("var_x", W, 2), ("var_y", W, 2)],
        "GETAI" => &[("var_triangle", W, 2)],
        "LSTMP" => &[("var", W, 1)],
        "MVIEF" => &[("var", W, 2)],
        "GWCOL" => &[("corner", R, 1), ("var_r", W, 1), ("var_g", W, 1), ("var_b", W, 1)],
        "SIN" | "COS" => &[("a", R, 2), ("b", R, 2), ("c", R, 2), ("var", W, 2)],
        "BTRLD" => &[("var", U, 1)],
        "CHGLD" | "CHMPH" => &[("var1", U, 1), ("var2", U, 1)],
        "MENU" => &[("param", U, 1)],
        "MPARA" | "RDMSD" => &[("value", U, 1)],
        "MPRA2" => &[("value", U, 2)],
        "CHMST" => &[("var", U, 1)],
        "CMTRA" => &[("ap0", R, 1), ("ap1", R, 1), ("ap2", R, 1), ("ap3", R, 1), ("materia", R, 1), ("var_quantity", U, 1)],
        "SETX" => &[("value", U, 1), ("a", U, 2), ("b", U, 1)],
        "GETX" => &[("value", U, 1), ("a", U, 2), ("var", U, 1)],
        "SEARCHX" => &[("search_start", U, 1), ("start", U, 2), ("end", U, 2), ("value", U, 1), ("var_result", U, 1)],
        "STITM" | "DLITM" | "CKITM" => &[("item", R, 2), ("quantity", R, 1)],
        "GOLDu" | "GOLDd" => &[("amount", R, 4)],
        "WNUMB" => &[("value", R, 4)],
        "SMTRA" | "DMTRA" => &[("materia", R, 1), ("ap0", R, 1), ("ap1", R, 1), ("ap2", R, 1)],
        "STTIM" => &[("hours", R, 1), ("minutes", R, 1), ("seconds", R, 1)],
        "MPu" | "MPd" | "HPu" | "HPd" => &[("party", R, 1), ("value", R, 2)],
        "SWCOL" => &[("corner", R, 1), ("r", R, 1), ("g", R, 1), ("b", R, 1)],
        "SHAKE" => &[("x_amplitude", R, 1), ("x_frames", R, 1), ("y_amplitude", R, 1), ("y_frames", R, 1)],
        "SCRLC" => &[("speed", R, 2)],
        "SCRLA" => &[("speed", R, 2), ("entity", R, 1)],
        "SCRLP" => &[("speed", R, 2), ("party", R, 1)],
        "SCR2D" => &[("x", R, 2), ("y", R, 2)],
        "SCR2DC" | "SCR2DL" => &[("x", R, 2), ("y", R, 2), ("speed", R, 2)],
        "VWOFT" => &[("unknown1", R, 2), ("unknown2", R, 2)],
        "FADE" => &[("r", R, 1), ("g", R, 1), ("b", R, 1)],
        "NFADE" => &[("r", R, 1), ("g", R, 1), ("b", R, 1), ("speed", R, 2)],
        "BGPDH" => &[("z", R, 2)],
        "BGSCR" => &[("x", R, 2), ("y", R, 2)],
        "BGON" | "BGOFF" => &[("group", R, 1), ("frame", R, 1)],
        "BGROL" | "BGROL2" | "BGCLR" => &[("group", R, 1)],
        "STPAL" | "LDPAL" | "CPPAL" | "RTPAL" | "ADPAL" | "MPPAL" | "MPPAL2" | "CPPAL2" | "RTPAL2" | "ADPAL2" => PALETTE,
        "BATTLE" => &[("battle", R, 2)],
        "MSPED" | "ASPED" => &[("speed", R, 2)],
        "DIR" | "TURNGEN" | "TURN" => &[("direction", R, 1)],
        "TALKR" | "SLIDR" => &[("radius", R, 1)],
        "TLKR2" | "SLDR2" => &[("radius", R, 2)],
        "MOVE" | "CMOVE" | "FMOVE" => &[("x", R, 2), ("y", R, 2)],
        "XYZI" => &[("x", R, 2), ("y", R, 2), ("z", R, 2), ("triangle", R, 2)],
        "XYI" => &[("x", R, 2), ("y", R, 2), ("triangle", R, 2)],
        "XYZ" => &[("x", R, 2), ("y", R, 2), ("z", R, 2)],
        "JUMP" => &[("x", R, 2), ("y", R, 2), ("triangle", R, 2), ("height", R, 2)],
        "LADER" => &[("x", R, 2), ("y", R, 2), ("z", R, 2), ("triangle", R, 2)],
        "OFST" => &[("x", R, 2), ("y", R, 2), ("z", R, 2), ("speed", R, 2)],
        "SLINE" => &[("x1", R, 2), ("y1", R, 2), ("z1", R, 2), ("x2", R, 2), ("y2", R, 2), ("z2", R, 2)],
        "SPLIT" => &[("xa", R, 2), ("ya", R, 2), ("da", R, 1), ("xb", R, 2), ("yb", R, 2), ("db", R, 1)],
        "SOUND" => &[("id", R, 2), ("pan", R, 1)],
        "AKAO" => &[("p1", R, 1), ("p2", R, 2), ("p3", R, 2), ("p4", R, 2), ("p5", R, 2)],
        "AKAO2" => &[("p1", R, 2), ("p2", R, 2), ("p3", R, 2), ("p4", R, 2), ("p5", R, 2)],
        "CMUSC" => &[("param1", R, 2), ("param2", R, 2)],
        "SMSPD" => &[("speed", R, 1)],
        _ => &[],
    }
}

/// The story moment in operand `o`: the word at `Var[2][0]` (its low byte alone for a byte compare).
fn is_moment(o: &Operand) -> bool {
    o.bank == MOMENT.0 && (o.value & 0xFF) == MOMENT.1 as i32
}

/// Address of a banked operand: the low byte of its stored number.
fn address(o: &Operand) -> usize {
    usize::try_from(o.value & 0xFF).unwrap_or(0)
}

/// The symbol of an ordered comparison code (`op` 0-5).
fn symbol(op: i32) -> Option<&'static str> {
    Some(match op {
        0 => "==",
        1 => "!=",
        2 => ">",
        3 => "<",
        4 => ">=",
        5 => "<=",
        _ => return None,
    })
}

/// `literal <sym> moment` read from the moment's side.
fn mirror(sym: &'static str) -> &'static str {
    match sym {
        ">" => "<",
        "<" => ">",
        ">=" => "<=",
        "<=" => ">=",
        same => same,
    }
}

/// `moment <symbol> literal` when the instruction compares the moment with a literal, either way round: the symbol, the
/// literal, and whether the compare reads the whole word (`IFSW`, `IFUW`) or only the low byte (`IFUB`).
fn moment_compare(ins: &Instruction) -> Option<(&'static str, i32, bool)> {
    if !COMPARES.contains(&ins.mnemonic) {
        return None;
    }
    let (left, right) = (ins.operand("left")?, ins.operand("right")?);
    let sym = symbol(ins.literal("op")?)?;
    let word = !matches!(ins.mnemonic, "IFUB" | "IFUBL");
    if is_moment(left) && right.bank == 0 {
        Some((sym, right.value, word))
    } else if is_moment(right) && left.bank == 0 {
        Some((mirror(sym), left.value, word))
    } else {
        None
    }
}

/// A variable as `Var[b][a]`, a literal as its number.
fn text(o: &Operand) -> String {
    if o.bank == 0 {
        o.value.to_string()
    } else {
        format!("Var[{}][{}]", o.bank, o.value & 0xFF)
    }
}

/// Every operand of `ins` as `name=value` (the jump target and the sub-opcode aside).
fn operand_list(ins: &Instruction) -> String {
    ins.operands.iter().filter(|o| !matches!(o.name, "jump" | "sub")).map(|o| format!("{}={}", o.name, text(o))).collect::<Vec<_>>().join(" ")
}

/// A comparison as it reads: `Var[2][0] >= 40`, `Var[3][208] bit 2 on`.
fn compare_text(ins: &Instruction) -> String {
    let (Some(l), Some(r)) = (ins.operand("left"), ins.operand("right")) else { return operand_list(ins) };
    let (l, r) = (text(l), text(r));
    match ins.literal("op") {
        Some(op) => match symbol(op) {
            Some(sym) => format!("{l} {sym} {r}"),
            None => match op {
                6 => format!("{l} & {r} != 0"),
                7 => format!("{l} ^ {r} != 0"),
                8 => format!("{l} | {r} != 0"),
                9 => format!("{l} bit {r} on"),
                10 => format!("{l} bit {r} off"),
                _ => format!("{l} op{op} {r}"),
            },
        },
        None => format!("{l} op? {r}"),
    }
}

/// What an access does, for the table: the comparison, or the operands.
fn detail(ins: &Instruction) -> String {
    if COMPARES.contains(&ins.mnemonic) {
        compare_text(ins)
    } else {
        operand_list(ins)
    }
}

/// A non-moment guard, for the table.
fn describe_guard(ins: &Instruction) -> String {
    if COMPARES.contains(&ins.mnemonic) {
        return compare_text(ins);
    }
    let list = operand_list(ins);
    if list.is_empty() {
        ins.mnemonic.to_string()
    } else {
        format!("{} {list}", ins.mnemonic)
    }
}

/// One byte of a variable that an instruction touches: its address in `bank`, the bits it reads or writes (`mask`), and
/// what it does with them.
#[derive(Debug, Clone, Copy)]
struct ByteTouch {
    bank: u8,
    addr: usize,
    mask: u8,
    kind: Kind,
}

/// The bytes (address, bit mask) that operand `o` of `ins` touches. A word operand touches two bytes. The bit operations
/// touch the one bit they name (`BITON` at `A` with bit `n` touches byte `A + n/8`, bit `n%8`, as the VM does), a mask test
/// the bits of its literal, and everything else the whole byte or bytes.
fn byte_masks(ins: &Instruction, key: &str, o: &Operand, width: usize) -> Vec<(usize, u8)> {
    let base = address(o);
    let whole = || (0..width).map(|i| (base + i, 0xFF)).collect::<Vec<_>>();
    match (key, o.name) {
        ("BITON" | "BITOFF" | "BITXOR", "var") => match ins.literal("bit") {
            Some(n) => {
                let n = n.rem_euclid(256) as usize;
                vec![(base + n / 8, 1u8 << (n % 8))]
            }
            // a bit number that is itself a variable: any bit of the 32 bytes from here
            None => (0..32).map(|i| (base + i, 0xFF)).collect(),
        },
        ("HBYTE", "var") => vec![(base + 1, 0xFF)],
        (_, "left") if COMPARES.contains(&key) => {
            let bits = 8 * width as i32;
            match (ins.literal("op"), ins.literal("right")) {
                (Some(6..=8), Some(m)) => (0..width).map(|i| (base + i, ((m >> (8 * i)) & 0xFF) as u8)).filter(|&(_, mask)| mask != 0).collect(),
                (Some(9 | 10), Some(n)) if (0..bits).contains(&n) => {
                    let n = n as usize;
                    vec![(base + n / 8, 1u8 << (n % 8))]
                }
                _ => whole(),
            }
        }
        _ => whole(),
    }
}

/// The variable bytes `ins` touches, with the table's kind and width. An operand the table does not name is `Unknown` and
/// one byte, and is recorded in `unclassified`.
fn touches(ins: &Instruction, key: &str, unclassified: &mut BTreeSet<String>) -> Vec<ByteTouch> {
    let mut out = Vec::new();
    for o in ins.operands.iter().filter(|o| o.bank != 0) {
        let (kind, width) = match roles(key).iter().find(|r| r.0 == o.name) {
            Some(r) => (r.1, r.2),
            None => {
                unclassified.insert(format!("{key}.{}", o.name));
                (Kind::Unknown, 1)
            }
        };
        for (addr, mask) in byte_masks(ins, key, o, width) {
            out.push(ByteTouch { bank: o.bank, addr, mask, kind });
        }
    }
    out
}

/// Whether a jump from outside the stretch guard `g` covers can land in it, past `g` and up to `k`: then `k` can run
/// without `g` having been taken.
fn bypassable(list: &[Instruction], g: usize, k: usize) -> bool {
    let (from, to) = (list[g].end(), list[k].offset);
    list.iter().enumerate().any(|(i, ins)| (i < g || i >= k) && ins.is_jump() && ins.jump_target.is_some_and(|t| from <= t && t <= to))
}

/// The guards of `list[k]` (`guards_of` in `story.rs`): the forward jumps before it whose target lies beyond it, minus
/// the unconditional `JMPF`/`JMPFL`, which is no test.
struct Guards {
    moment: Vec<String>,
    other: Vec<String>,
    /// Some moment test guards it and no jump from outside can skip that test.
    guarded: bool,
    /// The guarding moment test includes an equality (`moment==N`), the kind that holds at one moment only.
    eq: bool,
}

fn guards(list: &[Instruction], k: usize) -> Guards {
    let at = list[k].offset;
    let mut g = Guards { moment: Vec::new(), other: Vec::new(), guarded: false, eq: false };
    for (j, ins) in list[..k].iter().enumerate() {
        let forward = ins.is_jump() && !matches!(ins.opcode, 0x10 | 0x11) && ins.jump_target.is_some_and(|t| t > at);
        if !forward || ins.end() > at {
            continue;
        }
        let bypass = bypassable(list, j, k);
        let mark = if bypass { " (bypassable)" } else { "" };
        match moment_compare(ins) {
            Some((sym, lit, word)) => {
                let who = if word { "moment" } else { "moment(low byte)" };
                g.moment.push(format!("{who}{sym}{lit}{mark}"));
                if !bypass {
                    g.guarded = true;
                    g.eq |= sym == "==";
                }
            }
            None => g.other.push(format!("{}{mark}", describe_guard(ins))),
        }
    }
    g
}

/// One access of a variable by one instruction.
struct Hit {
    field: String,
    entity: usize,
    entity_name: String,
    slot: usize,
    offset: usize,
    mnemonic: String,
    kind: Kind,
    /// The bits of the variable's byte this access reads or writes (`0xFF`: the whole byte).
    mask: u8,
    detail: String,
    moment: Vec<String>,
    other: Vec<String>,
    guarded: bool,
    eq: bool,
}

#[derive(Default)]
struct Stats {
    fields: usize,
    scripts: usize,
    shared: usize,
    partial: usize,
    instructions: usize,
    unclassified: BTreeSet<String>,
    unreadable: Vec<String>,
}

/// Walk every script slot of one field (each distinct code span once) and record the accesses of the variables.
fn scan_field(field: &str, script: &FieldScript, vars: &[(u8, u8)], hits: &mut [Vec<Hit>], stats: &mut Stats) {
    let mut seen = BTreeSet::new();
    for entity in 0..script.entity_count() {
        let entity_name = script.entity_name(entity).unwrap_or("").to_string();
        for slot in 0..SCRIPTS_PER_ENTITY {
            let Some(span) = script.span(entity, slot) else { continue };
            if !seen.insert(span.start) {
                stats.shared += 1;
                continue;
            }
            let Ok(d) = script.disassemble_partial(entity, slot) else { continue };
            stats.scripts += 1;
            stats.instructions += d.instructions.len();
            stats.partial += usize::from(d.error.is_some());
            let list = &d.instructions;
            for k in 0..list.len() {
                let ins = &list[k];
                let key = match ins.sub {
                    Some((_, name)) if ins.mnemonic == "SPECIAL" => name,
                    _ => ins.mnemonic,
                };
                let t = touches(ins, key, &mut stats.unclassified);
                for (vi, &(qb, qa)) in vars.iter().enumerate() {
                    let qa = usize::from(qa);
                    let mut hit: Option<(Kind, u8)> = None;
                    for b in t.iter().filter(|b| b.bank == qb && b.addr == qa && b.mask != 0) {
                        hit = Some(match hit {
                            None => (b.kind, b.mask),
                            Some((kind, mask)) => (kind.merge(b.kind), mask | b.mask),
                        });
                    }
                    let Some((kind, mask)) = hit else { continue };
                    let g = guards(list, k);
                    let shown = if key == ins.mnemonic { ins.mnemonic.to_string() } else { format!("{}.{key}", ins.mnemonic) };
                    hits[vi].push(Hit {
                        field: field.to_string(),
                        entity,
                        entity_name: entity_name.clone(),
                        slot,
                        offset: ins.offset,
                        mnemonic: shown,
                        kind,
                        mask,
                        detail: detail(ins),
                        moment: g.moment,
                        other: g.other,
                        guarded: g.guarded,
                        eq: g.eq,
                    });
                }
            }
        }
    }
}

fn field_set<'a>(hs: &[&'a Hit], pred: impl Fn(Kind) -> bool) -> BTreeSet<&'a str> {
    hs.iter().filter(|h| pred(h.kind)).map(|h| h.field.as_str()).collect()
}

fn names(set: &BTreeSet<&str>) -> String {
    if set.is_empty() {
        "none".to_string()
    } else {
        set.iter().copied().collect::<Vec<_>>().join(", ")
    }
}

/// The verdict on a set of accesses (a whole variable, or the accesses that touch one bit of it).
fn verdict(hs: &[&Hit]) -> String {
    if hs.is_empty() {
        return "no field script touches it".to_string();
    }
    let readers = field_set(hs, Kind::reads);
    if readers.is_empty() {
        return "written only: no field script reads it".to_string();
    }
    let writers = field_set(hs, Kind::surely_writes);
    let other: Vec<&str> = readers.difference(&writers).copied().collect();
    if !other.is_empty() {
        return format!("READ IN OTHER FIELDS (save it): {}", other.join(", "));
    }
    let open: Vec<String> = hs.iter().filter(|h| !h.guarded).map(|h| format!("{} {:04X}", h.field, h.offset)).collect();
    if !open.is_empty() {
        return format!("only read where written, but {} access(es) not under a moment test: {}", open.len(), open.join(", "));
    }
    if hs.iter().all(|h| h.eq) {
        "only read where written, every access under an equality moment test".to_string()
    } else {
        "only read where written, every access under a moment test (some not equalities)".to_string()
    }
}

fn mask_text(mask: u8) -> String {
    match mask {
        0xFF => "byte".to_string(),
        m if m.count_ones() == 1 => format!("bit {}", m.trailing_zeros()),
        m => format!("bits {m:#04x}"),
    }
}

fn guard_column(h: &Hit) -> String {
    let mut parts: Vec<String> = h.moment.clone();
    if !h.guarded {
        parts.push("NOT under a moment test".to_string());
    }
    parts.extend(h.other.iter().map(|o| format!("if {o}")));
    parts.join("; ")
}

fn report(vars: &[(u8, u8)], hits: &[Vec<Hit>], stats: &Stats) {
    for (&(bank, addr), hs) in vars.iter().zip(hits) {
        let all: Vec<&Hit> = hs.iter().collect();
        let fields: BTreeSet<&str> = all.iter().map(|h| h.field.as_str()).collect();
        println!();
        println!("== Var[{bank}][{addr}]: {} access(es) in {} field(s)", all.len(), fields.len());
        for h in &all {
            println!(
                "  {:<15} e{:<2} {:<12} s{:<2} {:04X}  {:<2} {:<13} {:<9} {:<30} {}",
                h.field,
                h.entity,
                h.entity_name,
                h.slot,
                h.offset,
                h.kind.label(),
                h.mnemonic,
                mask_text(h.mask),
                h.detail,
                guard_column(h)
            );
        }
        let readers = field_set(&all, Kind::reads);
        let writers = field_set(&all, Kind::writes);
        println!("  read by {} field(s): {}", readers.len(), names(&readers));
        println!("  written by {} field(s): {}", writers.len(), names(&writers));
        println!("  verdict, whole value: {}", verdict(&all));
        if all.iter().any(|h| h.mask != 0xFF) {
            for b in 0..8u8 {
                let bit: Vec<&Hit> = all.iter().copied().filter(|h| h.mask & (1u8 << b) != 0).collect();
                if !bit.is_empty() {
                    println!("  verdict, bit {b}: {}", verdict(&bit));
                }
            }
        }
    }
    println!();
    println!(
        "scanned {} fields: {} distinct scripts ({} shared slots skipped, {} partly decoded), {} instructions",
        stats.fields, stats.scripts, stats.shared, stats.partial, stats.instructions
    );
    if stats.unclassified.is_empty() {
        println!("banked operands with no entry in the role table: none");
    } else {
        let list: Vec<String> = stats.unclassified.iter().cloned().collect();
        println!("banked operands with no entry in the role table (taken as unknown, one byte): {}", list.join(", "));
    }
    if !stats.unreadable.is_empty() {
        println!("unreadable fields: {}", stats.unreadable.len());
    }
}

/// `bank:address` with bank 1-15 (0 is a literal) and address 0-255.
fn parse_var(arg: &str) -> Option<(u8, u8)> {
    let (bank, address) = arg.split_once(':')?;
    let (bank, address): (u8, u8) = (bank.parse().ok()?, address.parse().ok()?);
    (1..=15).contains(&bank).then_some((bank, address))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let vars: Vec<(u8, u8)> = match args.iter().map(|a| parse_var(a)).collect::<Option<Vec<_>>>() {
        Some(v) if !v.is_empty() => v,
        _ => {
            eprintln!("usage: var_readers BANK:ADDRESS... (bank 1-15, address 0-255), e.g. 3:66 1:4");
            return;
        }
    };
    let Some(dir) = data_dir() else {
        eprintln!("FMV_FF7_DATA is not set; skipping (point it at the game's data/ directory)");
        return;
    };
    let path = flevel_path(&dir);
    let mut lgp = match LgpArchive::open_path(&path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("cannot open {}: {e}", path.display());
            return;
        }
    };
    let mut names: Vec<String> = lgp.list().iter().map(|e| e.name.clone()).filter(|n| is_field_name(n)).collect();
    names.sort();
    let mut hits: Vec<Vec<Hit>> = vars.iter().map(|_| Vec::new()).collect();
    let mut stats = Stats::default();
    for name in &names {
        let field = match load_field(&mut lgp, name) {
            Ok(f) => f,
            Err(e) => {
                stats.unreadable.push(format!("{name}: {e}"));
                continue;
            }
        };
        let script = match FieldScript::from_field(&field) {
            Ok(s) => s,
            Err(e) => {
                stats.unreadable.push(format!("{name}: {e}"));
                continue;
            }
        };
        stats.fields += 1;
        scan_field(name, &script, &vars, &mut hits, &mut stats);
    }
    report(&vars, &hits, &stats);
}
