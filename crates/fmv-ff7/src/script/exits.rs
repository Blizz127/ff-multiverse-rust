//! Exits that scripts decide: the `MAPJUMP`s of a field, who runs them and what they depend on.
//!
//! A field's twelve gateway slots ([`crate::field::Triggers::gateways`]) are the simple way out:
//! data, no script. Everything else that leaves a field is a `MAPJUMP` (opcode `0x60`, "Change
//! Field": field id, arrival `x`, `y`, triangle and direction) that some script runs. The FFRTT page
//! for `MAPJUMP` says a `LINE` entity (`D0`) "that fires `MAPJUMP` when the player walks over it"
//! lets a field have more than twelve exits, and the owned data agree. This module finds every
//! `MAPJUMP` with literal operands (all five are literals by construction: the opcode has no bank
//! nibbles) and works out, statically, three things about each:
//!
//! * **What sets it going** ([`ExitTrigger`]): the *root* script whose run can reach it. A root is
//!   a `LINE` entity's trigger script (slots 1 to 6), a model entity's talk or contact script
//!   (slots 1 and 2), or an entity's own script 0: its init part (up to the first `RET`, run once
//!   when the field loads) and its main part (after it, running from the first frame on). Scripts
//!   start each other with `REQ`, `REQSW` and `REQEW` (entity index and slot are literals), so the
//!   walk follows those calls ([`MAX_CALL_DEPTH`] deep, never into a script already on the path),
//!   and the [`ScriptExit::path`] lists the scripts from the root to the one that holds the jump.
//!   A `MAPJUMP` that no root reaches is reported as [`ExitTrigger::Orphan`], so that every
//!   `MAPJUMP` of [`crate::script::FieldScript::map_jumps`] appears at least once.
//! * **Where the line is** ([`ExitLine`]): for a `LINE` root, the end points of its line (the first
//!   `LINE` of script 0's init part, whose operands are literal) and whether it starts enabled
//!   (the last unconditional `LINON` of the init part; no `LINON` leaves it unset).
//! * **What it depends on** ([`ExitGuard`]): the `IF*` instructions whose body contains the `MAPJUMP`
//!   (or the `REQ` that leads to it), outermost first. In this instruction set a conditional jump
//!   falls through when its test is TRUE and jumps forward when it is FALSE (FFRTT `IFUB` page,
//!   confirmed on the owned data by every jump landing on an instruction start), so the code
//!   between the `IF` and its target runs only when the test holds. When the instruction just
//!   before the target is an unconditional forward jump (the usual `if / else` shape), the code
//!   between the target and *that* jump's target is the else branch and carries the negated test.
//!   Tests are reported as the script states them: variables as `Var[bank][address]` exactly like
//!   the disassembler prints them. **What the banks mean in the savemap is not decoded here**
//!   (UNVERIFIED which banks persist). `Var[2][0]` is compared with ever larger literals all over
//!   the owned data (10, 27, 33, ... 1 614), which fits a story progress counter; that reading
//!   rests on the pattern only, not on a source this crate checked.
//!
//! Nothing is evaluated here (`crate::exit_gate` judges the guards): the analysis is flow-insensitive within a script (it lists every
//! `MAPJUMP` the code contains, whether or not an earlier instruction would have returned), and the
//! guards are the nesting of the code, not a proof that a branch can be taken. A root that contains
//! no `MAPJUMP` on any chain simply produces nothing. The party-addressed requests (`PREQ`, `PRQSW`,
//! `PRQEW`) are not followed (they name a party slot, not an entity), and a `MAPJUMP` that only such a
//! call reaches is reported as an orphan.
//!
//! # Status: consumed by `crate::exit_gate`
//!
//! **The walk-on exits of this module are the area's exits** (`Ff7FieldArea::exits()` reports them after the gateways,
//! each with its guards kept for `AreaRuntime::exit_state`; see `crate::exit_gate` for which are kept and how the
//! guards are judged against an account's story). The rest stays description: talk and contact jumps, `[OK]` lines
//! and the automatic jumps of a director or a timer are not walk-on exits and nothing offers them.
//!
//! * **One line, several destinations.** The script often picks the destination itself: the
//!   elevator `elevtr1` has no gateway, one `LINE` (entity `jp0`, slot 3) and four `MAPJUMP`s under
//!   `Var[1][225]` bit 0 (the `ele` entity's script sets it as part of its ride, so it reads as the
//!   elevator's state) and `Var[2][0] < 33` (north or south reactor fields). The data hold 78 lines with two or more
//!   different destinations. They are several exits on one line, and the server takes the first that is open for the
//!   crossing player's story (`crate::exit_gate`), so a destination that belongs to later story stays closed
//!   because its guards say so.
//! * **What is a trigger.** Makou Reactor names a line entity's slots 1 to 6 `[OK]`, `Move`, `Move`,
//!   `Go`, `Go 1x` and `Go away` (`GrpScript::scriptName`, <https://github.com/myst6re/makoureactor>).
//!   *When* each runs is UNVERIFIED (nothing this crate could reach says). The data have jumps in
//!   slots 2 to 5 only; slot 1 presumably needs the OK button, so [`ExitTrigger::is_walk_on`] leaves
//!   it out.
//!
//! # Measured on the owned FF7 Steam Edition (en), 2026-11-08
//!
//! Over the 702 fields (`examples/exits_dump.rs --all`; a `MAPJUMP` that several roots reach is
//! counted once per root, the archive has 1 801 `MAPJUMP` instructions with literal operands): 2 511
//! script exits, by root: 547 talk, 84 contact, 629 main, 96 orphan, none from an init part, and
//! for lines 41 `[OK]`, 522 slot 2, 123 slot 3, 329 slot 4 (`Go`), 140 slot 5 (`Go 1x`), none in
//! slot 6. Walk-on exits (lines, slots 2 to 5): 1 114, of which 707 sit under a condition on the
//! game state, 94 have a placeholder arrival (`(0, 0)` on triangle 0), 11 belong to lines that start
//! disabled, and 78 lines have more than one destination.

use super::decode::Instruction;
use super::layout::{INIT_SCRIPT, SCRIPTS_PER_ENTITY};
use super::maplist::Maplist;
use super::FieldScript;
use std::collections::{btree_map, BTreeMap, BTreeSet};
use std::fmt;

/// Deepest chain of `REQ` calls followed from a root (the root's own script is depth 0).
pub const MAX_CALL_DEPTH: usize = 6;

/// Most scripts one field's walk may visit across all its roots (a bound on hostile data; real
/// fields need a few hundred).
const MAX_VISITS: usize = 200_000;

/// Most exits one field reports.
pub const MAX_SCRIPT_EXITS: usize = 4096;

const OP_RET: u8 = 0x00;
const OP_REQ: u8 = 0x01;
const OP_REQEW: u8 = 0x03;
const OP_JMPF: u8 = 0x10;
const OP_JMPFL: u8 = 0x11;
const OP_IFUB: u8 = 0x14;
const OP_IFUBL: u8 = 0x15;
const OP_IFSW: u8 = 0x16;
const OP_IFSWL: u8 = 0x17;
const OP_IFUW: u8 = 0x18;
const OP_IFUWL: u8 = 0x19;
const OP_IFKEY: u8 = 0x30;
const OP_IFKEYON: u8 = 0x31;
const OP_IFKEYOFF: u8 = 0x32;
const OP_MAPJUMP: u8 = 0x60;
const OP_CHAR: u8 = 0xA1;
const OP_IFPRTYQ: u8 = 0xCB;
const OP_IFMEMBQ: u8 = 0xCC;
const OP_LINE: u8 = 0xD0;
const OP_LINON: u8 = 0xD1;

/// The first and last trigger slots of a `LINE` entity (`[OK]`, `Move`, `Move`, `Go`, `Go 1x`,
/// `Go away`).
pub const LINE_SLOTS: std::ops::RangeInclusive<u8> = 1..=6;
/// The talk script slot of a model entity.
const TALK_SLOT: u8 = 1;
/// The contact script slot of a model entity.
const CONTACT_SLOT: u8 = 2;

/// One script of the field: entity index and slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ScriptRef {
    pub entity: usize,
    pub slot: u8,
}

/// What can start the chain of scripts that ends in a `MAPJUMP`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ExitTrigger {
    /// A `LINE` entity's trigger script `slot` (1 `[OK]`, 2 and 3 `Move`, 4 `Go`, 5 `Go 1x`,
    /// 6 `Go away`, Makou Reactor's names): the player on, crossing or leaving the line (UNVERIFIED
    /// which slot runs when).
    Line { slot: u8 },
    /// A model entity's talk script (slot 1): the player presses OK in front of it.
    Talk,
    /// A model entity's contact script (slot 2): the player walks into it.
    Contact,
    /// The entity's script 0 up to its first `RET`: runs once when the field loads. No player action.
    Init,
    /// The entity's script 0 after its first `RET`: runs from the first frame on, over and over.
    /// No player action; cut-scenes and timers live here.
    Main,
    /// A script that no root reaches by `REQ` (something the engine or another field starts).
    Orphan,
}

impl ExitTrigger {
    /// Walking over the line can presumably set it off: a line trigger other than `[OK]` (UNVERIFIED,
    /// see the module docs).
    pub fn is_walk_on(self) -> bool {
        matches!(self, Self::Line { slot } if slot != 1)
    }

    /// The player has to do something other than walk (press OK, talk, bump into a model).
    pub fn is_interaction(self) -> bool {
        matches!(self, Self::Line { slot: 1 } | Self::Talk | Self::Contact)
    }

    /// Nothing the player does starts it: the script runs on its own (a cut-scene, a timer).
    pub fn is_automatic(self) -> bool {
        matches!(self, Self::Init | Self::Main | Self::Orphan)
    }
}

impl fmt::Display for ExitTrigger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Line { slot } => {
                let name = match slot {
                    1 => "[OK]",
                    2 | 3 => "Move",
                    4 => "Go",
                    5 => "Go 1x",
                    6 => "Go away",
                    _ => "?",
                };
                write!(f, "line slot {slot} ({name})")
            }
            Self::Talk => f.write_str("talk"),
            Self::Contact => f.write_str("contact"),
            Self::Init => f.write_str("init"),
            Self::Main => f.write_str("main"),
            Self::Orphan => f.write_str("orphan"),
        }
    }
}

/// The line a `LINE` entity watches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExitLine {
    /// End points `(x, y, z)` in the field's walkmesh coordinates.
    pub a: [i16; 3],
    pub b: [i16; 3],
    /// The last unconditional `LINON` of the init part: `Some(true)` the line starts on,
    /// `Some(false)` it starts off (a script switches it on later), `None` the init part never says.
    pub enabled: Option<bool>,
}

/// A value a conditional compares: a literal, or a variable the script names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardValue {
    Literal(i32),
    /// Variable `address` of bank `bank` (as the disassembler prints it: `Var[bank][address]`).
    Var { bank: u8, address: u8 },
}

impl fmt::Display for GuardValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Literal(v) => write!(f, "{v}"),
            Self::Var { bank, address } => write!(f, "Var[{bank}][{address}]"),
        }
    }
}

/// The comparison byte of `IFUB`, `IFSW` and `IFUW` (FFRTT `IFUB` page).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompareOp {
    Eq,
    Ne,
    Gt,
    Lt,
    Ge,
    Le,
    /// `A & B` is non-zero.
    And,
    /// `A ^ B` is non-zero.
    Xor,
    /// `A | B` is non-zero.
    Or,
    /// Bit `B` of `A` is set (`A & (1 << B)`).
    BitOn,
    /// Bit `B` of `A` is clear.
    BitOff,
    /// A comparison byte the table does not name.
    Other(u8),
}

impl CompareOp {
    pub fn from_byte(b: u8) -> Self {
        match b {
            0 => Self::Eq,
            1 => Self::Ne,
            2 => Self::Gt,
            3 => Self::Lt,
            4 => Self::Ge,
            5 => Self::Le,
            6 => Self::And,
            7 => Self::Xor,
            8 => Self::Or,
            9 => Self::BitOn,
            10 => Self::BitOff,
            other => Self::Other(other),
        }
    }
}

/// How wide, and how signed, the compared values are (the three `IF` opcodes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompareWidth {
    /// `IFUB`: unsigned byte.
    Byte,
    /// `IFSW`: signed word.
    SignedWord,
    /// `IFUW`: unsigned word.
    Word,
}

/// How a key test reads the keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyTest {
    /// `IFKEY`: held.
    Held,
    /// `IFKEYON`: just pressed.
    Pressed,
    /// `IFKEYOFF`: just released.
    Released,
}

/// What an `IF*` instruction tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardTest {
    /// `IFUB`, `IFSW`, `IFUW` (and their long forms): `left op right`.
    Compare { width: CompareWidth, left: GuardValue, op: CompareOp, right: GuardValue },
    /// `IFKEY`, `IFKEYON`, `IFKEYOFF`: the keys in `mask`.
    Keys { mask: u16, test: KeyTest },
    /// `IFPRTYQ`: the character is in the party.
    InParty { character: u8 },
    /// `IFMEMBQ`: the character is a member (can be put in the party).
    IsMember { character: u8 },
}

/// One condition the code of an exit sits under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExitGuard {
    /// Section-relative offset of the `IF*` instruction.
    pub offset: usize,
    pub test: GuardTest,
    /// The code is in the *else* branch: it runs when the test FAILS.
    pub negated: bool,
}

impl ExitGuard {
    /// The test reads the keys (player input), not the game state.
    pub fn is_input(&self) -> bool {
        matches!(self.test, GuardTest::Keys { .. })
    }

    /// The variables the test reads, in the order the script names them.
    pub fn variables(&self) -> Vec<(u8, u8)> {
        match self.test {
            GuardTest::Compare { left, right, .. } => [left, right]
                .into_iter()
                .filter_map(|v| match v {
                    GuardValue::Var { bank, address } => Some((bank, address)),
                    GuardValue::Literal(_) => None,
                })
                .collect(),
            _ => Vec::new(),
        }
    }
}

impl fmt::Display for ExitGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self.test {
            GuardTest::Compare { left, op, right, .. } => {
                let binary = |sym: &str| format!("{left} {sym} {right}");
                match op {
                    CompareOp::Eq => binary("=="),
                    CompareOp::Ne => binary("!="),
                    CompareOp::Gt => binary(">"),
                    CompareOp::Lt => binary("<"),
                    CompareOp::Ge => binary(">="),
                    CompareOp::Le => binary("<="),
                    CompareOp::And => binary("&"),
                    CompareOp::Xor => binary("^"),
                    CompareOp::Or => binary("|"),
                    CompareOp::BitOn => binary("has bit"),
                    CompareOp::BitOff => binary("lacks bit"),
                    CompareOp::Other(b) => binary(&format!("op{b}")),
                }
            }
            GuardTest::Keys { mask, test } => {
                let how = match test {
                    KeyTest::Held => "held",
                    KeyTest::Pressed => "pressed",
                    KeyTest::Released => "released",
                };
                format!("keys {mask:#06x} {how}")
            }
            GuardTest::InParty { character } => format!("character {character} in the party"),
            GuardTest::IsMember { character } => format!("character {character} is a member"),
        };
        if self.negated {
            write!(f, "not ({text})")
        } else {
            f.write_str(&text)
        }
    }
}

/// One `MAPJUMP` of the field, with how it gets run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptExit {
    pub trigger: ExitTrigger,
    /// The entity whose script is the root of the chain.
    pub entity: usize,
    pub entity_name: String,
    /// The root's line (for [`ExitTrigger::Line`]).
    pub line: Option<ExitLine>,
    /// The scripts from the root (first) to the one that holds the jump (last): one more for every
    /// `REQ` followed.
    pub path: Vec<ScriptRef>,
    /// Section-relative offset of the `MAPJUMP`.
    pub offset: usize,
    /// Destination field id (index into the maplist) and its name (`None`: past the end of the list).
    pub target_id: u16,
    pub target: Option<String>,
    /// Where the player stands after the jump, in the TARGET's coordinates, and the direction byte
    /// (256 to a turn, see [`crate::field_direction_to_facing`]).
    pub x: i16,
    pub y: i16,
    /// The fourth operand: the target walkmesh triangle (the page calls it "Z").
    pub triangle: i16,
    pub direction: u8,
    /// Every condition the chain passes through (outermost first, the caller's before the callee's).
    pub guards: Vec<ExitGuard>,
}

impl ScriptExit {
    /// The arrival is `(0, 0)` on triangle 0: the shape of the placeholders (315 of the 1 801 literal
    /// `MAPJUMP`s of the owned data), where the target's own scripts put the player.
    pub fn arrival_is_placeholder(&self) -> bool {
        self.x == 0 && self.y == 0 && self.triangle == 0
    }

    /// Some condition of the chain reads the game state (not just the keys): what a story layer
    /// would gate on.
    pub fn is_story_gated(&self) -> bool {
        self.guards.iter().any(|g| !g.is_input())
    }

    /// The conditions that read the game state.
    pub fn story_guards(&self) -> impl Iterator<Item = &ExitGuard> {
        self.guards.iter().filter(|g| !g.is_input())
    }
}

/// A decoded script, with the regions its conditionals guard.
struct Code {
    instructions: Vec<Instruction>,
    /// Offset of the first `RET` (the end of script 0's init part).
    first_ret: Option<usize>,
    regions: Vec<Region>,
}

/// `[from, to)` runs only under `guard`.
struct Region {
    from: usize,
    to: usize,
    guard: ExitGuard,
}

impl Code {
    fn new(instructions: Vec<Instruction>) -> Self {
        let first_ret = instructions.iter().find(|i| i.opcode == OP_RET).map(|i| i.offset);
        let by_end: BTreeMap<usize, usize> = instructions.iter().enumerate().map(|(i, ins)| (ins.end(), i)).collect();
        let mut regions = Vec::new();
        for ins in &instructions {
            let Some(test) = guard_test(ins) else { continue };
            let Some(target) = ins.jump_target.filter(|&t| t >= ins.end()) else { continue };
            let guard = ExitGuard { offset: ins.offset, test, negated: false };
            regions.push(Region { from: ins.end(), to: target, guard });
            // `if / else`: the instruction that ends at the target is a forward jump over the else part.
            let over = by_end
                .get(&target)
                .and_then(|&i| instructions.get(i))
                .filter(|j| matches!(j.opcode, OP_JMPF | OP_JMPFL) && j.offset >= ins.end())
                .and_then(|j| j.jump_target)
                .filter(|&end| end > target);
            if let Some(end) = over {
                regions.push(Region { from: target, to: end, guard: ExitGuard { negated: true, ..guard } });
            }
        }
        Self { instructions, first_ret, regions }
    }

    /// The conditions the instruction at `offset` sits under, outermost (earliest `IF`) first.
    fn guards_at(&self, offset: usize) -> Vec<ExitGuard> {
        let mut guards: Vec<ExitGuard> = self.regions.iter().filter(|r| r.from <= offset && offset < r.to).map(|r| r.guard).collect();
        guards.sort_by_key(|g| (g.offset, g.negated));
        guards
    }
}

fn value_of(ins: &Instruction, name: &str) -> Option<GuardValue> {
    let o = ins.operand(name)?;
    Some(if o.bank == 0 { GuardValue::Literal(o.value) } else { GuardValue::Var { bank: o.bank, address: u8::try_from(o.value & 0xFF).unwrap_or(0) } })
}

fn guard_test(ins: &Instruction) -> Option<GuardTest> {
    let compare = |width| {
        Some(GuardTest::Compare {
            width,
            left: value_of(ins, "left")?,
            op: CompareOp::from_byte(u8::try_from(ins.literal("op")?).unwrap_or(u8::MAX)),
            right: value_of(ins, "right")?,
        })
    };
    let keys = |test| Some(GuardTest::Keys { mask: u16::try_from(ins.literal("keys")?).unwrap_or(0), test });
    let character = || u8::try_from(ins.literal("character")?).ok();
    match ins.opcode {
        OP_IFUB | OP_IFUBL => compare(CompareWidth::Byte),
        OP_IFSW | OP_IFSWL => compare(CompareWidth::SignedWord),
        OP_IFUW | OP_IFUWL => compare(CompareWidth::Word),
        OP_IFKEY => keys(KeyTest::Held),
        OP_IFKEYON => keys(KeyTest::Pressed),
        OP_IFKEYOFF => keys(KeyTest::Released),
        OP_IFPRTYQ => Some(GuardTest::InParty { character: character()? }),
        OP_IFMEMBQ => Some(GuardTest::IsMember { character: character()? }),
        _ => None,
    }
}

/// Which instructions of a script a walk starts from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part {
    Whole,
    /// Script 0 up to and including its first `RET`.
    Init,
    /// Script 0 after its first `RET`.
    Main,
}

impl Part {
    fn contains(self, offset: usize, first_ret: Option<usize>) -> bool {
        match (self, first_ret) {
            (Self::Whole, _) => true,
            (Self::Init, Some(ret)) => offset <= ret,
            (Self::Init, None) => true,
            (Self::Main, Some(ret)) => offset > ret,
            (Self::Main, None) => false,
        }
    }
}

/// A `MAPJUMP` found on a walk.
struct Found {
    path: Vec<ScriptRef>,
    offset: usize,
    operands: [i32; 5],
    guards: Vec<ExitGuard>,
}

/// The literal operands of a `MAPJUMP` (all five are literals by construction, but a damaged
/// instruction may lack one).
fn mapjump_operands(ins: &Instruction) -> Option<[i32; 5]> {
    Some([ins.literal("field")?, ins.literal("x")?, ins.literal("y")?, ins.literal("triangle")?, ins.literal("direction")?])
}

struct Walker<'a> {
    script: &'a FieldScript<'a>,
    codes: &'a BTreeMap<usize, Code>,
    /// Scripts visited so far, over all roots (see [`MAX_VISITS`]).
    visits: usize,
    /// How many more `MAPJUMP`s may be recorded (see [`MAX_SCRIPT_EXITS`]).
    budget: usize,
    found: Vec<Found>,
}

impl Walker<'_> {
    fn code_of(&self, at: ScriptRef) -> Option<&Code> {
        let span = self.script.span(at.entity, usize::from(at.slot))?;
        self.codes.get(&span.start)
    }

    /// Record a `MAPJUMP` unless the budget is spent.
    fn record(&mut self, path: Vec<ScriptRef>, offset: usize, operands: [i32; 5], guards: Vec<ExitGuard>) {
        if self.budget > 0 {
            self.budget -= 1;
            self.found.push(Found { path, offset, operands, guards });
        }
    }

    /// Visit script `at` (only `part` of it), recording its `MAPJUMP`s and following its `REQ`s.
    fn walk(&mut self, at: ScriptRef, part: Part, path: &mut Vec<ScriptRef>, guards: &mut Vec<ExitGuard>) {
        self.visits += 1;
        if self.visits > MAX_VISITS || self.budget == 0 {
            return;
        }
        let Some(code) = self.code_of(at) else { return };
        // `code` borrows `self`, and the walk below needs `&mut self`: take what it needs first.
        let first_ret = code.first_ret;
        let hits: Vec<(Instruction, Vec<ExitGuard>)> = code
            .instructions
            .iter()
            .filter(|i| (i.opcode == OP_MAPJUMP || (OP_REQ..=OP_REQEW).contains(&i.opcode)) && part.contains(i.offset, first_ret))
            .map(|i| (i.clone(), code.guards_at(i.offset)))
            .collect();
        path.push(at);
        for (ins, local) in hits {
            let mark = guards.len();
            guards.extend(local);
            if ins.opcode == OP_MAPJUMP {
                if let Some(operands) = mapjump_operands(&ins) {
                    self.record(path.clone(), ins.offset, operands, guards.clone());
                }
            } else if path.len() <= MAX_CALL_DEPTH {
                let target = ins.literal("entity").and_then(|e| usize::try_from(e).ok()).zip(ins.literal("script").and_then(|s| u8::try_from(s).ok()));
                if let Some((entity, slot)) = target {
                    let next = ScriptRef { entity, slot };
                    if entity < self.script.entity_count() && usize::from(slot) < SCRIPTS_PER_ENTITY && !path.contains(&next) {
                        self.walk(next, Part::Whole, path, guards);
                    }
                }
            }
            guards.truncate(mark);
        }
        path.pop();
    }
}

/// The first `LINE` of script 0's init part and the last unconditional `LINON` before the first
/// `RET`, or `None` when the entity is not a line entity (or its `LINE` has no literal operands).
fn line_of(code: &Code) -> Option<ExitLine> {
    let init = |i: &&Instruction| Part::Init.contains(i.offset, code.first_ret);
    let line = code.instructions.iter().filter(init).find(|i| i.opcode == OP_LINE)?;
    let c = |name: &str| i16::try_from(line.literal(name)?).ok();
    let (a, b) = ([c("x1")?, c("y1")?, c("z1")?], [c("x2")?, c("y2")?, c("z2")?]);
    let enabled = code
        .instructions
        .iter()
        .filter(init)
        .filter(|i| i.opcode == OP_LINON && code.guards_at(i.offset).is_empty())
        .filter_map(|i| i.literal("enabled"))
        .next_back()
        .map(|v| v != 0);
    Some(ExitLine { a, b, enabled })
}

/// Whether the init part of script 0 runs `CHAR` (a field model).
fn is_model(code: &Code) -> bool {
    code.instructions.iter().any(|i| i.opcode == OP_CHAR && Part::Init.contains(i.offset, code.first_ret))
}

/// What the roots found so far: the exits, and which `MAPJUMP`s they cover.
#[derive(Default)]
struct Collected {
    exits: Vec<ScriptExit>,
    /// `(start of the holding script, offset of the MAPJUMP)` of everything reported.
    reached: BTreeSet<(usize, usize)>,
}

impl Collected {
    /// Turn what the walker found since the last call into exits of root `trigger` of `entity`.
    fn take(&mut self, walker: &mut Walker<'_>, maplist: &Maplist, trigger: ExitTrigger, entity: usize, line: Option<ExitLine>) {
        for f in std::mem::take(&mut walker.found) {
            if let Some(holder) = f.path.last().and_then(|r| walker.script.span(r.entity, usize::from(r.slot))) {
                self.reached.insert((holder.start, f.offset));
            }
            let [field, x, y, triangle, direction] = f.operands;
            let target_id = u16::try_from(field).unwrap_or(u16::MAX);
            self.exits.push(ScriptExit {
                trigger,
                entity,
                entity_name: walker.script.entity_name(entity).unwrap_or("").to_string(),
                line,
                path: f.path,
                offset: f.offset,
                target_id,
                target: maplist.name(target_id).map(str::to_string),
                x: i16::try_from(x).unwrap_or(0),
                y: i16::try_from(y).unwrap_or(0),
                triangle: i16::try_from(triangle).unwrap_or(0),
                direction: u8::try_from(direction).unwrap_or(0),
                guards: f.guards,
            });
        }
    }
}

impl FieldScript<'_> {
    /// Every `MAPJUMP` with literal operands in the field's scripts, with the root script that runs
    /// it, the line it belongs to (line roots), the scripts it passes through and the conditions
    /// it sits under. See the module docs for what "root", "path" and "guard" mean and for the
    /// limits (nothing is evaluated).
    ///
    /// Results are in root order: for each entity in index order, its script 0 init part, its main
    /// part, then its line or talk and contact slots; orphans come last. A jump reached by two
    /// chains from one root is reported once per chain (their guards differ). At most
    /// [`MAX_SCRIPT_EXITS`] are reported.
    pub fn script_exits(&self, maplist: &Maplist) -> Vec<ScriptExit> {
        // Decode every distinct script once (slots that share an entry point share a span).
        let mut codes: BTreeMap<usize, Code> = BTreeMap::new();
        let mut first_ref: BTreeMap<usize, ScriptRef> = BTreeMap::new();
        for entity in 0..self.entity_count() {
            for slot in 0..SCRIPTS_PER_ENTITY {
                let (Some(span), Ok(slot_u8)) = (self.span(entity, slot), u8::try_from(slot)) else { continue };
                if let btree_map::Entry::Vacant(v) = codes.entry(span.start) {
                    if let Ok(d) = self.disassemble_partial(entity, slot) {
                        v.insert(Code::new(d.instructions));
                        first_ref.insert(span.start, ScriptRef { entity, slot: slot_u8 });
                    }
                }
            }
        }
        let mut walker = Walker { script: self, codes: &codes, visits: 0, budget: MAX_SCRIPT_EXITS, found: Vec::new() };
        let mut out = Collected::default();
        let init_slot = u8::try_from(INIT_SCRIPT).unwrap_or(0);
        for entity in 0..self.entity_count() {
            let root = ScriptRef { entity, slot: init_slot };
            let (line, model) = walker.code_of(root).map_or((None, false), |c| (line_of(c), is_model(c)));
            let mut roots: Vec<(ExitTrigger, ScriptRef, Part)> = vec![(ExitTrigger::Init, root, Part::Init), (ExitTrigger::Main, root, Part::Main)];
            if line.is_some() {
                roots.extend(LINE_SLOTS.map(|slot| (ExitTrigger::Line { slot }, ScriptRef { entity, slot }, Part::Whole)));
            } else if model {
                roots.push((ExitTrigger::Talk, ScriptRef { entity, slot: TALK_SLOT }, Part::Whole));
                roots.push((ExitTrigger::Contact, ScriptRef { entity, slot: CONTACT_SLOT }, Part::Whole));
            }
            for (trigger, at, part) in roots {
                walker.walk(at, part, &mut Vec::new(), &mut Vec::new());
                let line = if matches!(trigger, ExitTrigger::Line { .. }) { line } else { None };
                out.take(&mut walker, maplist, trigger, entity, line);
            }
        }
        // MAPJUMPs no root reaches: one report each, from the first slot that points at their script.
        for (start, code) in &codes {
            let Some(&at) = first_ref.get(start) else { continue };
            for ins in code.instructions.iter().filter(|i| i.opcode == OP_MAPJUMP) {
                if out.reached.contains(&(*start, ins.offset)) {
                    continue;
                }
                if let Some(operands) = mapjump_operands(ins) {
                    walker.record(vec![at], ins.offset, operands, code.guards_at(ins.offset));
                    out.take(&mut walker, maplist, ExitTrigger::Orphan, at.entity, None);
                }
            }
        }
        out.exits
    }
}

#[cfg(test)]
mod tests {
    use super::super::synth::SynthScript;
    use super::*;

    fn maplist(names: &[&str]) -> Maplist {
        let mut v = (names.len() as u16).to_le_bytes().to_vec();
        for n in names {
            let mut rec = [0u8; super::super::MAPLIST_NAME_LEN];
            rec[..n.len()].copy_from_slice(n.as_bytes());
            v.extend_from_slice(&rec);
        }
        Maplist::parse(&v).unwrap()
    }

    // --- tiny assembler for the opcodes the tests need ---

    const RET: u8 = 0x00;

    fn s16(v: i16) -> [u8; 2] {
        v.to_le_bytes()
    }

    fn line(a: (i16, i16, i16), b: (i16, i16, i16)) -> Vec<u8> {
        let mut v = vec![OP_LINE];
        for c in [a.0, a.1, a.2, b.0, b.1, b.2] {
            v.extend_from_slice(&s16(c));
        }
        v
    }

    fn linon(on: u8) -> Vec<u8> {
        vec![OP_LINON, on]
    }

    fn mapjump(field: u16, x: i16, y: i16, tri: i16, dir: u8) -> Vec<u8> {
        let mut v = vec![OP_MAPJUMP];
        v.extend_from_slice(&field.to_le_bytes());
        v.extend_from_slice(&s16(x));
        v.extend_from_slice(&s16(y));
        v.extend_from_slice(&s16(tri));
        v.push(dir);
        v
    }

    fn req(entity: u8, slot: u8) -> Vec<u8> {
        vec![OP_REQ, entity, (6 << 5) | slot]
    }

    /// `IFSW Var[bank][addr] op literal`, skipping `body_len` bytes of code when it fails.
    fn ifsw_var(bank: u8, addr: u8, op: u8, literal: i16, body_len: usize) -> Vec<u8> {
        // layout: opcode, bank byte (left in the high nibble, right literal = 0), left (var address, 2 bytes),
        // right (2 bytes), op, jump (distance counts from the jump byte itself, which is skipped too)
        let mut v = vec![OP_IFSW, bank << 4];
        v.extend_from_slice(&u16::from(addr).to_le_bytes());
        v.extend_from_slice(&s16(literal));
        v.push(op);
        v.push(u8::try_from(body_len + 1).unwrap());
        v
    }

    fn jmpf(skip: usize) -> Vec<u8> {
        vec![OP_JMPF, u8::try_from(skip + 1).unwrap()]
    }

    fn cat(parts: &[Vec<u8>]) -> Vec<u8> {
        parts.concat()
    }

    fn line_entity_init(on: Option<u8>) -> Vec<u8> {
        let mut parts = vec![line((100, 0, 0), (300, 0, 0))];
        if let Some(on) = on {
            parts.push(linon(on));
        }
        parts.push(vec![RET]);
        cat(&parts)
    }

    fn exits_of(s: &SynthScript, names: &[&str]) -> Vec<ScriptExit> {
        let built = s.build();
        FieldScript::parse(&built).unwrap().script_exits(&maplist(names))
    }

    #[test]
    fn a_line_slot_with_a_mapjump_is_a_walk_on_exit_with_its_line() {
        let jump = mapjump(2, -40, 55, 9, 64);
        let s = SynthScript::default()
            .entity("hero", vec![(0, vec![RET])])
            .entity("door", vec![(0, line_entity_init(Some(1))), (4, cat(&[jump.clone(), vec![RET]]))]);
        let exits = exits_of(&s, &["here", "there", "yonder"]);
        assert_eq!(exits.len(), 1, "{exits:#?}");
        let e = &exits[0];
        assert_eq!(e.trigger, ExitTrigger::Line { slot: 4 });
        assert!(e.trigger.is_walk_on() && !e.trigger.is_interaction() && !e.trigger.is_automatic());
        assert_eq!((e.entity, e.entity_name.as_str()), (1, "door"));
        assert_eq!(e.line, Some(ExitLine { a: [100, 0, 0], b: [300, 0, 0], enabled: Some(true) }));
        assert_eq!((e.target_id, e.target.as_deref()), (2, Some("yonder")));
        assert_eq!((e.x, e.y, e.triangle, e.direction), (-40, 55, 9, 64));
        assert_eq!(e.path, [ScriptRef { entity: 1, slot: 4 }]);
        assert!(e.guards.is_empty() && !e.is_story_gated() && !e.arrival_is_placeholder());
    }

    #[test]
    fn the_line_slots_are_told_apart_and_a_field_without_a_jump_has_none() {
        let s = SynthScript::default()
            .entity("a", vec![(0, line_entity_init(None)), (1, cat(&[mapjump(1, 1, 1, 1, 0), vec![RET]])), (6, cat(&[mapjump(2, 2, 2, 2, 0), vec![RET]]))])
            .entity("b", vec![(0, vec![RET])]);
        let exits = exits_of(&s, &["x", "y", "z"]);
        let triggers: Vec<ExitTrigger> = exits.iter().map(|e| e.trigger).collect();
        assert_eq!(triggers, [ExitTrigger::Line { slot: 1 }, ExitTrigger::Line { slot: 6 }]);
        assert!(exits[0].trigger.is_interaction() && !exits[0].trigger.is_walk_on(), "[OK] needs the button");
        assert!(exits[1].trigger.is_walk_on(), "Go away is movement");
        assert_eq!(exits[0].line.unwrap().enabled, None, "no LINON: the init part says nothing");
        assert!(exits_of(&SynthScript::default().entity("a", vec![(0, vec![RET])]), &[]).is_empty());
    }

    #[test]
    fn a_line_that_starts_switched_off_says_so() {
        let s = SynthScript::default().entity("l", vec![(0, line_entity_init(Some(0))), (5, cat(&[mapjump(1, 0, 0, 0, 0), vec![RET]]))]);
        let exits = exits_of(&s, &["a", "b"]);
        assert_eq!(exits[0].line.unwrap().enabled, Some(false));
        assert!(exits[0].arrival_is_placeholder());
    }

    #[test]
    fn guards_are_the_conditionals_around_the_jump_and_else_branches_are_negated() {
        // Go: if Var[2][0] >= 27 { MAPJUMP -> 1 } else { MAPJUMP -> 2 }
        let then = cat(&[mapjump(1, 10, 10, 1, 0), jmpf(mapjump(2, 0, 0, 0, 0).len())]);
        let go = cat(&[ifsw_var(2, 0, 4, 27, then.len()), then, mapjump(2, 20, 20, 2, 0), vec![RET]]);
        let s = SynthScript::default().entity("l", vec![(0, line_entity_init(Some(1))), (4, go)]);
        let exits = exits_of(&s, &["a", "b", "c"]);
        assert_eq!(exits.len(), 2, "{exits:#?}");
        let (yes, no) = (&exits[0], &exits[1]);
        assert_eq!(yes.target.as_deref(), Some("b"));
        assert_eq!(no.target.as_deref(), Some("c"));
        assert_eq!(yes.guards.len(), 1);
        let g = yes.guards[0];
        assert!(!g.negated);
        assert_eq!(g.test, GuardTest::Compare { width: CompareWidth::SignedWord, left: GuardValue::Var { bank: 2, address: 0 }, op: CompareOp::Ge, right: GuardValue::Literal(27) });
        assert_eq!(g.to_string(), "Var[2][0] >= 27");
        assert_eq!(g.variables(), [(2, 0)]);
        assert_eq!(no.guards.len(), 1);
        assert!(no.guards[0].negated, "the else branch runs when the test fails");
        assert_eq!(no.guards[0].offset, g.offset);
        assert_eq!(no.guards[0].to_string(), "not (Var[2][0] >= 27)");
        assert!(yes.is_story_gated() && no.is_story_gated());
        assert_eq!(yes.story_guards().count(), 1);
    }

    #[test]
    fn guards_nest_outermost_first_and_a_jump_after_the_conditional_is_unguarded() {
        let inner = cat(&[mapjump(1, 1, 1, 1, 0)]);
        let outer_body = cat(&[ifsw_var(2, 0, 0, 5, inner.len()), inner.clone()]);
        let go = cat(&[ifsw_var(1, 7, 1, 3, outer_body.len()), outer_body, mapjump(2, 2, 2, 2, 0), vec![RET]]);
        let s = SynthScript::default().entity("l", vec![(0, line_entity_init(Some(1))), (4, go)]);
        let exits = exits_of(&s, &["a", "b", "c"]);
        assert_eq!(exits.len(), 2);
        let texts: Vec<String> = exits[0].guards.iter().map(ToString::to_string).collect();
        assert_eq!(texts, ["Var[1][7] != 3", "Var[2][0] == 5"]);
        assert!(exits[1].guards.is_empty(), "past both conditionals: nothing guards it");
        assert!(!exits[1].is_story_gated());
    }

    #[test]
    fn a_req_chain_carries_the_callers_guards_and_the_path() {
        // The line's Go script: if Var[2][0] < 9 { REQ entity 2, slot 3 }. Entity 2's slot 3: MAPJUMP.
        let call = req(2, 3);
        let go = cat(&[ifsw_var(2, 0, 3, 9, call.len()), call, vec![RET]]);
        let s = SynthScript::default()
            .entity("hero", vec![(0, vec![RET])])
            .entity("line", vec![(0, line_entity_init(Some(1))), (4, go)])
            .entity("dir", vec![(0, vec![RET]), (3, cat(&[mapjump(1, 5, 6, 7, 8), vec![RET]]))]);
        let exits = exits_of(&s, &["a", "b"]);
        // The director's slot 3 is reached from the line (the walk) and is not an orphan.
        assert_eq!(exits.len(), 1, "{exits:#?}");
        let e = &exits[0];
        assert_eq!(e.trigger, ExitTrigger::Line { slot: 4 });
        assert_eq!(e.entity, 1);
        assert_eq!(e.path, [ScriptRef { entity: 1, slot: 4 }, ScriptRef { entity: 2, slot: 3 }]);
        assert_eq!(e.guards.iter().map(ToString::to_string).collect::<Vec<_>>(), ["Var[2][0] < 9"]);
        assert_eq!((e.x, e.y, e.triangle, e.direction), (5, 6, 7, 8));
    }

    #[test]
    fn model_scripts_and_the_entitys_own_init_and_main_parts_are_not_walk_on_exits() {
        // talk: a model entity (CHAR) whose talk script jumps; auto: script 0 jumps in its main part;
        // init: script 0 jumps before its first RET; orphan: a slot nothing starts.
        let model_init = vec![OP_CHAR, 0, RET];
        let s = SynthScript::default()
            .entity("npc", vec![(0, model_init), (1, cat(&[mapjump(1, 1, 1, 1, 0), vec![RET]])), (2, cat(&[mapjump(2, 2, 2, 2, 0), vec![RET]]))])
            .entity("dir", vec![(0, cat(&[mapjump(3, 3, 3, 3, 0), vec![RET], mapjump(4, 4, 4, 4, 0), vec![RET]])), (9, cat(&[mapjump(5, 5, 5, 5, 0), vec![RET]]))]);
        let exits = exits_of(&s, &["a", "b", "c", "d", "e", "f"]);
        let by_target: BTreeMap<u16, ExitTrigger> = exits.iter().map(|e| (e.target_id, e.trigger)).collect();
        assert_eq!(by_target[&1], ExitTrigger::Talk);
        assert_eq!(by_target[&2], ExitTrigger::Contact);
        assert_eq!(by_target[&3], ExitTrigger::Init);
        assert_eq!(by_target[&4], ExitTrigger::Main);
        assert_eq!(by_target[&5], ExitTrigger::Orphan);
        assert_eq!(exits.len(), 5);
        assert!(exits.iter().all(|e| !e.trigger.is_walk_on() && e.line.is_none()));
        assert!(by_target[&1].is_interaction() && by_target[&2].is_interaction());
        assert!(by_target[&3].is_automatic() && by_target[&4].is_automatic() && by_target[&5].is_automatic());
    }

    #[test]
    fn a_req_cycle_ends_and_a_target_outside_the_field_is_ignored() {
        // slot 4 requests slot 5, which requests slot 4 again; slot 5 also requests a missing entity
        let s = SynthScript::default().entity(
            "l",
            vec![
                (0, line_entity_init(Some(1))),
                (4, cat(&[req(0, 5), vec![RET]])),
                (5, cat(&[req(0, 4), req(9, 1), mapjump(1, 1, 1, 1, 0), vec![RET]])),
            ],
        );
        let exits = exits_of(&s, &["a", "b"]);
        assert!(exits.iter().any(|e| e.trigger == ExitTrigger::Line { slot: 4 } && e.path.len() == 2), "{exits:#?}");
        assert!(exits.iter().all(|e| e.path.len() <= MAX_CALL_DEPTH + 1));
    }

    #[test]
    fn an_unknown_field_id_keeps_its_number_and_has_no_name() {
        let s = SynthScript::default().entity("l", vec![(0, line_entity_init(Some(1))), (4, cat(&[mapjump(500, 1, 2, 3, 4), vec![RET]]))]);
        let exits = exits_of(&s, &["only"]);
        assert_eq!((exits[0].target_id, exits[0].target.as_deref()), (500, None));
    }

    #[test]
    fn the_guard_texts_cover_every_test_and_operator() {
        let var = GuardValue::Var { bank: 5, address: 20 };
        let g = |test| ExitGuard { offset: 0, test, negated: false };
        let cmp = |op| g(GuardTest::Compare { width: CompareWidth::Byte, left: var, op, right: GuardValue::Literal(7) });
        assert_eq!(cmp(CompareOp::BitOn).to_string(), "Var[5][20] has bit 7");
        assert_eq!(cmp(CompareOp::BitOff).to_string(), "Var[5][20] lacks bit 7");
        assert_eq!(cmp(CompareOp::Le).to_string(), "Var[5][20] <= 7");
        assert_eq!(cmp(CompareOp::Other(77)).to_string(), "Var[5][20] op77 7");
        assert_eq!(CompareOp::from_byte(200), CompareOp::Other(200));
        let keys = g(GuardTest::Keys { mask: 0x20, test: KeyTest::Pressed });
        assert!(keys.is_input() && keys.variables().is_empty());
        assert_eq!(keys.to_string(), "keys 0x0020 pressed");
        assert_eq!(g(GuardTest::InParty { character: 3 }).to_string(), "character 3 in the party");
        assert_eq!(g(GuardTest::IsMember { character: 4 }).to_string(), "character 4 is a member");
        assert!(!g(GuardTest::InParty { character: 3 }).is_input());
    }

    #[test]
    fn damaged_scripts_never_panic() {
        // every truncation and a few byte flips of a small section go through the whole analysis
        let jump = mapjump(1, 2, 3, 4, 5);
        let go = cat(&[ifsw_var(2, 0, 4, 27, jump.len()), jump, req(0, 5), vec![RET]]);
        let built = SynthScript::default().entity("l", vec![(0, line_entity_init(Some(1))), (4, go.clone()), (5, go)]).build();
        let names = maplist(&["a", "b"]);
        for cut in 0..built.len() {
            if let Ok(s) = FieldScript::parse(&built[..cut]) {
                let _ = s.script_exits(&names);
            }
        }
        for at in 0..built.len() {
            let mut b = built.clone();
            b[at] ^= 0xFF;
            if let Ok(s) = FieldScript::parse(&b) {
                let _ = s.script_exits(&names);
            }
        }
    }
}
