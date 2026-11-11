//! The bounded, straight-line run of a field script that [`super`] reads sound from.
//!
//! It is not a script engine. It follows ONE path through a script, the one the story and the held keys
//! decide, and says what the sound opcodes on that path do and when (in frames of the field's 30 per second):
//!
//! * `IF*` is judged against the story ([`StoryState`]'s `known_*`: the moment and the tracked bits) and the keys
//!   the run was told are held. True falls through, false takes the jump, and anything that cannot be judged
//!   **stops the run** and is counted ([`Unjudged`]): nothing is guessed.
//! * `WAIT n` adds `n` frames. `REQ` and `REQSW` start another script at the same time, `REQEW` waits for it to end
//!   (a callee that never ends, or ends at a time nobody knows, stops the caller too).
//! * An opcode on the short list of instant ones ([`instant`]) costs no time and changes nothing the run reads. Any
//!   other opcode (a walk, a jump, an animation that is waited for, a window the player closes) **stops the run**
//!   there: how long it takes, and what it leaves behind, is not in the script.
//! * A backward jump stops the run too ([`Stop::IdleLoop`]): the script is waiting, or repeating, for something the
//!   run cannot see. What lies before the jump has been run once; the loop is not run round again, and the
//!   channels its body plays on stay **open** (see [`RunInfo::open`]): the second pass could change them.
//! * Variables are not tracked: a script's own writes are not remembered, so a guard on a variable the story does
//!   not carry stops the run even if the script set it a moment ago. A write to a variable the story DOES carry (the
//!   moment, a tracked bit) stops the run too, because from there the story's value is no longer the script's.
//!
//! Like the rest of the crate: no indexing, no unchecked arithmetic and no panics on script bytes, and every run is
//! bounded ([`MAX_STEPS`], [`MAX_CALL_DEPTH`], [`MAX_FRAMES`]).

use std::collections::BTreeMap;
use std::rc::Rc;

use crate::script::{CompareOp, CompareWidth, ExitGuard, FieldScript, GuardTest, GuardValue, Instruction, KeyTest, ScriptRef, SCRIPTS_PER_ENTITY};
use crate::story::{tracked_var, StoryState, MOMENT_ADDRESS, MOMENT_BANK};

/// Instructions one run may execute across all the scripts it calls (a real init part is a few dozen).
pub const MAX_STEPS: usize = 4_000;
/// How deep `REQ` calls are followed (the same bound as the exits' walk).
pub const MAX_CALL_DEPTH: usize = 6;
/// The longest a run may last, in frames (five minutes at 30 per second): a script that waits longer is not an
/// ambience or an effect, it is something else.
pub const MAX_FRAMES: u32 = 30 * 60 * 5;
/// The most events one machine keeps (a pathological script cannot grow a plan without bound).
const MAX_EVENTS: usize = 4_096;
/// The AKAO operations that play a sound effect on channel 1 to 4 (the forum's "40-43: play sound effect on channel
/// 1, 2, 3, 4"; see the module docs of [`super`]).
const AKAO_PLAY_FIRST: i32 = 40;
const AKAO_PLAY_LAST: i32 = 43;

/// Which opcode played a sound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// `SOUND` (`F1`): always effect channel 1.
    Sound,
    /// `AKAO` (`F2`) with operation 40 to 43.
    Akao,
    /// `AKAO2` (`DA`) with operation 40 to 43 (the same operands in 16-bit fields).
    Akao2,
}

/// One sound play or stop on an effect channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Call {
    /// Effect channel, 1 to 4.
    pub channel: u8,
    /// The game's sound id (1-based: entry `id - 1` of `audio.fmt`); **0 stops the channel**.
    pub id: u16,
    /// Pan: 0 left, 63 or 64 centre, 127 right (the script's own byte).
    pub pan: u8,
    pub source: Source,
}

/// A sound opcode whose id or pan names a variable: the value is not in the script.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Banked;

impl Call {
    /// The play or stop that `ins` is, when it is one of the sound opcodes with literal operands: `Ok(None)` for any
    /// other instruction (and for an `AKAO` operation that is not a sound play: music, volume, pan sweeps),
    /// `Err(Banked)` for a sound opcode whose operands name variables.
    pub fn of(ins: &Instruction) -> Result<Option<Self>, Banked> {
        let literal = |name: &str| ins.literal(name);
        let (channel, id, pan, source) = match ins.opcode {
            OP_SOUND => (1, literal("id"), literal("pan"), Source::Sound),
            OP_AKAO | OP_AKAO2 => {
                let op = match literal("op") {
                    Some(op) if (AKAO_PLAY_FIRST..=AKAO_PLAY_LAST).contains(&op) => op,
                    Some(_) => return Ok(None),
                    None => return Err(Banked),
                };
                let source = if ins.opcode == OP_AKAO { Source::Akao } else { Source::Akao2 };
                (op - AKAO_PLAY_FIRST + 1, literal("p2"), literal("p1"), source)
            }
            _ => return Ok(None),
        };
        let (Some(id), Some(pan)) = (id, pan) else { return Err(Banked) };
        let (Ok(channel), Ok(id), Ok(pan)) = (u8::try_from(channel), u16::try_from(id), u8::try_from(pan.clamp(0, 255))) else { return Err(Banked) };
        Ok(Some(Self { channel, id, pan, source }))
    }

    /// The bit of this call's channel in an `open` mask.
    pub fn channel_bit(&self) -> u8 {
        1u8.checked_shl(u32::from(self.channel.saturating_sub(1))).unwrap_or(0)
    }
}

const OP_RET: u8 = 0x00;
const OP_REQ: u8 = 0x01;
const OP_REQEW: u8 = 0x03;
const OP_JMPF: u8 = 0x10;
const OP_JMPFL: u8 = 0x11;
const OP_JMPB: u8 = 0x12;
const OP_JMPBL: u8 = 0x13;
const OP_IFUB: u8 = 0x14;
const OP_IFUBL: u8 = 0x15;
const OP_IFSW: u8 = 0x16;
const OP_IFSWL: u8 = 0x17;
const OP_IFUW: u8 = 0x18;
const OP_IFUWL: u8 = 0x19;
const OP_WAIT: u8 = 0x24;
const OP_IFKEY: u8 = 0x30;
const OP_IFKEYON: u8 = 0x31;
const OP_IFKEYOFF: u8 = 0x32;
const OP_MAPJUMP: u8 = 0x60;
const OP_BATTLE: u8 = 0x70;
const OP_IFPRTYQ: u8 = 0xCB;
const OP_IFMEMBQ: u8 = 0xCC;
const OP_AKAO2: u8 = 0xDA;
const OP_SOUND: u8 = 0xF1;
const OP_AKAO: u8 = 0xF2;

/// An event of a run: a sound play or stop, and when and where it happens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timed {
    /// Frames since the run began (30 per second).
    pub at: u32,
    pub call: Call,
    /// The script that holds the opcode and the opcode's section-relative offset.
    pub by: ScriptRef,
    pub offset: usize,
    /// Which run made it (an index into the machine's runs).
    pub run: usize,
}

/// Why a run stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    /// Native party resources require an execution model this sound runner does not yet have.
    NativePartyUnsupported,
    /// The script ended (`RET`, or it fell off its end into the `RET` that follows it): everything it does is known.
    Returned,
    /// A backward jump: the script is waiting or repeating (the loop's body is not run).
    IdleLoop { at: usize },
    /// The script leaves the field (`MAPJUMP`), starts a battle or a mini-game, or ends the game.
    LeavesField { at: usize },
    /// A condition the run cannot judge (see [`Unjudged`]).
    Unjudged { at: usize },
    /// An opcode the run cannot time or follow (not on the instant list), a write to a variable the story carries, or
    /// a call it cannot follow.
    Unknown { at: usize, opcode: u8 },
    /// The step, depth, frame or event budget ran out.
    Budget,
    /// The script cannot be decoded, or a jump leaves it.
    Broken,
}

impl Stop {
    /// A short word for tallies and logs.
    pub fn label(&self) -> &'static str {
        match self {
            Self::NativePartyUnsupported => "unsupported native party",
            Self::Returned => "returned",
            Self::IdleLoop { .. } => "idle loop",
            Self::LeavesField { .. } => "leaves the field",
            Self::Unjudged { .. } => "unjudged condition",
            Self::Unknown { .. } => "untimed opcode",
            Self::Budget => "budget",
            Self::Broken => "broken",
        }
    }

    /// Everything the script does is known: it returned, or it left the field.
    pub fn is_clean(&self) -> bool {
        matches!(self, Self::Returned | Self::LeavesField { .. })
    }
}

/// A condition a run could not judge: the script, where, and the test as the script states it
/// (`Var[5][1] <= 11`: numbers and variable names, never game text).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unjudged {
    pub by: ScriptRef,
    pub offset: usize,
    pub test: String,
}

/// What one run (one script, one part of it, started by the machine or by a `REQ`) came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunInfo {
    pub at: ScriptRef,
    pub parent: Option<usize>,
    pub stop: Stop,
    /// How long it took, when it ended in a way that says (a return).
    pub frames: Option<u32>,
    /// The channels (bit `channel - 1`) that sound opcodes the run did not reach could still change: the ones that
    /// play in the rest of the script after an unclean stop, or inside an idle loop.
    pub open: u8,
}

/// The channels that stay open for the events of `run`: its own and every ancestor's (a script that started it may
/// still change them).
pub fn open_for(runs: &[RunInfo], run: usize) -> u8 {
    let (mut mask, mut at, mut hops) = (0u8, Some(run), 0usize);
    while let Some(i) = at {
        let Some(info) = runs.get(i) else { break };
        mask |= info.open;
        at = info.parent;
        hops += 1;
        if hops > MAX_CALL_DEPTH + 2 {
            break;
        }
    }
    mask
}

/// The part of a script a run covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    /// The whole script (every slot but 0).
    Whole,
    /// Script 0 from its entry point to its first `RET`: runs once when the field loads.
    Init,
    /// Script 0 after its first `RET`: the entity's own logic from the first frame on.
    Main,
}

/// What a run is told about the world.
#[derive(Debug, Clone, Copy)]
pub struct Env<'a> {
    pub story: &'a StoryState,
    /// The keys held (FFRTT's key masks: `KEY_OK` 0x20, ...): what `IF*KEY*` is judged against.
    pub keys: u32,
}

/// A decoded script with an index from offsets to instructions.
struct Code {
    instructions: Vec<Instruction>,
    index: BTreeMap<usize, usize>,
    /// Index of the first `RET` in linear order.
    first_ret: Option<usize>,
    /// The section offset one past the last instruction.
    end: usize,
}

impl Code {
    fn new(instructions: Vec<Instruction>) -> Self {
        let index = instructions.iter().enumerate().map(|(i, ins)| (ins.offset, i)).collect();
        let first_ret = instructions.iter().position(|i| i.opcode == OP_RET);
        let end = instructions.last().map_or(0, Instruction::end);
        Self { instructions, index, first_ret, end }
    }

    /// Where a forward jump of `ins` (at index `pc`) goes: the instruction index, or the end of the script.
    fn forward(&self, ins: &Instruction, pc: usize) -> Jump {
        match ins.jump_target {
            Some(target) => match self.index.get(&target) {
                Some(&i) if i > pc => Jump::To(i),
                Some(_) => Jump::Broken,
                // The first byte after the script is the same as falling off its end.
                None if target >= self.end => Jump::End,
                None => Jump::Broken,
            },
            None => Jump::Broken,
        }
    }
}

enum Jump {
    To(usize),
    End,
    Broken,
}

enum Verdict {
    True,
    False,
    Unknown,
}

/// The test an `IF*` instruction makes, in the exits' terms (so that it prints the way theirs do).
fn guard_test(ins: &Instruction) -> Option<GuardTest> {
    let value = |name: &str| -> Option<GuardValue> {
        let o = ins.operand(name)?;
        Some(if o.bank == 0 { GuardValue::Literal(o.value) } else { GuardValue::Var { bank: o.bank, address: u8::try_from(o.value & 0xFF).unwrap_or(0) } })
    };
    let compare = |width| {
        Some(GuardTest::Compare {
            width,
            left: value("left")?,
            op: CompareOp::from_byte(u8::try_from(ins.literal("op")?).unwrap_or(u8::MAX)),
            right: value("right")?,
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

/// Judge a test against the story and the held keys.
fn judge(test: &GuardTest, env: &Env<'_>) -> Verdict {
    let yes = |b: bool| if b { Verdict::True } else { Verdict::False };
    match *test {
        GuardTest::Keys { mask, test } => {
            let hit = env.keys & u32::from(mask) != 0;
            match test {
                // A key is held or just pressed exactly when it is one of the keys the run was told are held.
                KeyTest::Held | KeyTest::Pressed => yes(hit),
                // "Just released" is false while the key is held; with nothing held there is nothing to say.
                KeyTest::Released if hit => Verdict::False,
                KeyTest::Released => Verdict::Unknown,
            }
        }
        GuardTest::InParty { .. } | GuardTest::IsMember { .. } => Verdict::Unknown,
        GuardTest::Compare { width, left, op, right } => {
            // A bit test on a variable the story carries only some bits of asks for exactly that bit.
            if matches!(op, CompareOp::BitOn | CompareOp::BitOff) {
                let want_set = matches!(op, CompareOp::BitOn);
                let bit = match right {
                    GuardValue::Literal(b) => u8::try_from(b).ok().filter(|b| *b < 8),
                    GuardValue::Var { .. } => None,
                };
                return match (left, bit) {
                    (GuardValue::Var { bank, address }, Some(bit)) => env.story.known_bit(bank, address, bit).map_or(Verdict::Unknown, |on| yes(on == want_set)),
                    (GuardValue::Literal(a), Some(bit)) => yes(((a >> u32::from(bit)) & 1 == 1) == want_set),
                    _ => Verdict::Unknown,
                };
            }
            let read = |v: GuardValue| -> Option<i64> {
                match v {
                    GuardValue::Literal(x) => Some(i64::from(x)),
                    GuardValue::Var { bank, address } => match width {
                        CompareWidth::Byte => env.story.known_byte(bank, address).map(i64::from),
                        CompareWidth::Word => env.story.known_word(bank, address).map(i64::from),
                        CompareWidth::SignedWord => env.story.known_word(bank, address).map(|w| i64::from(i16::from_le_bytes(w.to_le_bytes()))),
                    },
                }
            };
            let (Some(a), Some(b)) = (read(left), read(right)) else { return Verdict::Unknown };
            match op {
                CompareOp::Eq => yes(a == b),
                CompareOp::Ne => yes(a != b),
                CompareOp::Gt => yes(a > b),
                CompareOp::Lt => yes(a < b),
                CompareOp::Ge => yes(a >= b),
                CompareOp::Le => yes(a <= b),
                CompareOp::And => yes(a & b != 0),
                CompareOp::Xor => yes(a ^ b != 0),
                CompareOp::Or => yes(a | b != 0),
                CompareOp::BitOn | CompareOp::BitOff | CompareOp::Other(_) => Verdict::Unknown,
            }
        }
    }
}

/// Opcodes that cost no time and whose effect a run does not read (see the module docs): the writes and reads of
/// variables, the set-ups of an entity, the scene and its windows, the music and sound opcodes, the background and
/// palette changes, and the scrolls, fades, shakes, turns and animations that are only STARTED (the `...W` and `!`
/// opcodes that wait for them are not on the list). Everything else stops a run: a walk, a jump, a ladder, a window
/// the player closes, a wait for a scroll.
pub fn instant(opcode: u8) -> bool {
    matches!(opcode,
        // scene and entity set-ups: SPECIAL, BLINK, BGMOVIE, KAWAI, SLIP, BGPDH, BGSCR, UC, MENU2, BTLON, BTLMD, TLKON
        0x0F | 0x26 | 0x27 | 0x28 | 0x2B | 0x2C | 0x2D | 0x33 | 0x4A | 0x71 | 0x72 | 0x7E
        // variables and what reads the world into one: GETPC, PXYZI, the saturating forms, RDMSD, SETBYTE to SEARCHX,
        // PGTDR, LSTMP, GETDIR, GETAXY, GETAI, AXYZI, SIN, COS
        | 0x73..=0x7D | 0x7F | 0x80..=0x9F | 0x6E | 0xB7..=0xB9 | 0xC1 | 0xD4 | 0xD5
        // the entity's own state: PC, CHAR, DFANM, ANIME1, VISI, XYZI, XYI, XYZ, ANIME2, CANIM1, MSPED, DIR, TURNGEN,
        // TURN, DIRA, CANIM2, ASPED, CC, OFST, TALKR, SLIDR, SOLID, LINE, LINON, MPJPO, SLINE, TLKR2, SLDR2, PMJMP,
        // PMJMP2, FCFIX, CCANM, ANIMB, IDLCK
        | 0xA0..=0xA4 | 0xA5..=0xA7 | 0xAE | 0xB0 | 0xB2..=0xB6 | 0xBB | 0xBD | 0xBF | 0xC3 | 0xC5..=0xC7 | 0xD0..=0xD3 | 0xD6..=0xD9
        | 0xDB | 0xDC | 0xDD | 0x6D
        // music and sound: AKAO2, MUSIC, SOUND, AKAO, MUSVT, MUSVM, MULCK, BMUSC, FMUSC, CMUSC
        | 0xDA | 0xF0..=0xF6 | 0xFC | 0xFD
        // windows and text set-ups that wait for nobody, names, tables, the numbers in a window
        | 0x2E | 0x2F | 0x36 | 0x37 | 0x41..=0x43 | 0x45 | 0x47 | 0x4B | 0x4D | 0x4F | 0x50..=0x55 | 0x57
        // background and palette changes and scroll, fade and shake STARTS: BGON to ADPAL2, MPPAL, SCRLC to SCR2DC,
        // SCR2DL, MPDSP, VWOFT, FADE, SCRLP, SCRLO, SHAKE, NFADE
        | 0xE0..=0xEF | 0xDF | 0x61..=0x66 | 0x68 | 0x69 | 0x6A | 0x6B | 0x6F | 0x5E | 0x25
        // the party, items, materia and money the scripts hand out: PDIRA, SPTYE, GTPYE, STTIM, GOLDu to CHGLD, STITM to CMTRA,
        // PRTYP to PRTYE, MMBud to MMBUK
        | 0x34 | 0x0A | 0x0B | 0x38..=0x3B | 0x58..=0x5D | 0xC8..=0xCA | 0xCD..=0xCF
    )
}

/// The opcodes that end the field (or this script's part in it): `MAPJUMP`, `BATTLE`, `MINIGAME`, `GAMEOVER`.
fn leaves(opcode: u8) -> bool {
    matches!(opcode, OP_MAPJUMP | OP_BATTLE | 0x20 | 0xFF)
}

/// Whether the story carries the byte at `bank`/`address` (the moment's two bytes, or a tracked variable).
fn story_carries(bank: u8, address: u8) -> bool {
    (bank == MOMENT_BANK && address <= MOMENT_ADDRESS.saturating_add(1)) || tracked_var(bank, address).is_some()
}

/// Whether `ins` writes a variable the story carries (a destination operand `var` or `var_*` that is a variable of such a
/// byte, or of the byte before it, which a word write reaches).
fn writes_story(ins: &Instruction) -> bool {
    ins.operands.iter().filter(|o| o.bank != 0 && (o.name == "var" || o.name.starts_with("var_"))).any(|o| {
        let address = u8::try_from(o.value & 0xFF).unwrap_or(0);
        story_carries(o.bank, address) || story_carries(o.bank, address.saturating_add(1))
    })
}

/// What a finished run reports to whoever called it.
struct End {
    stop: Stop,
    /// The run's own length, if it ended in a return.
    frames: Option<u32>,
}

impl End {
    fn unknown(ins: &Instruction) -> Self {
        Self { stop: Stop::Unknown { at: ins.offset, opcode: ins.opcode }, frames: None }
    }
}

/// The machine: it decodes each script once and runs parts of scripts, collecting their events.
pub struct Machine<'a> {
    script: &'a FieldScript<'a>,
    env: Env<'a>,
    codes: BTreeMap<usize, Option<Rc<Code>>>,
    steps: usize,
    /// Every play or stop, in the order the machine met them.
    pub events: Vec<Timed>,
    /// Every run, in the order they began.
    pub runs: Vec<RunInfo>,
    /// The conditions that stopped runs.
    pub unjudged: Vec<Unjudged>,
    /// Sound opcodes whose operands name variables (skipped, counted).
    pub banked: u32,
}

impl<'a> Machine<'a> {
    pub fn new(script: &'a FieldScript<'a>, env: Env<'a>) -> Self {
        Self { script, env, codes: BTreeMap::new(), steps: 0, events: Vec::new(), runs: Vec::new(), unjudged: Vec::new(), banked: 0 }
    }

    /// The decoded script `at`, if it has code (a script that stops decoding early still gives what it has).
    fn code(&mut self, at: ScriptRef) -> Option<Rc<Code>> {
        let span = self.script.span(at.entity, usize::from(at.slot))?;
        if let Some(found) = self.codes.get(&span.start) {
            return found.clone();
        }
        let decoded = self.script.disassemble_partial(at.entity, usize::from(at.slot)).ok().map(|d| Rc::new(Code::new(d.instructions)));
        self.codes.insert(span.start, decoded.clone());
        decoded
    }

    /// Run `part` of script `at` from the start of the part (or from the instruction at section offset `from`),
    /// beginning `t0` frames after whatever the caller counts from. The events and the run's record stay in the
    /// machine. Returns the record's index.
    pub fn run(&mut self, at: ScriptRef, part: Part, from: Option<usize>, t0: u32) -> usize {
        // Party writers are ignored by this planner. Accepting a native-party story could
        // therefore select sound from stale raw slots after an authored party change.
        if self.env.story.supports_native_party() {
            let id = self.runs.len();
            self.runs.push(RunInfo { at, parent: None, stop: Stop::NativePartyUnsupported, frames: None, open: 0b1111 });
            return id;
        }
        self.steps = 0;
        let mut stack = vec![at];
        self.run_inner(at, part, from, t0, None, &mut stack).0
    }

    fn run_inner(&mut self, at: ScriptRef, part: Part, from: Option<usize>, t0: u32, parent: Option<usize>, stack: &mut Vec<ScriptRef>) -> (usize, Option<u32>) {
        let id = self.runs.len();
        self.runs.push(RunInfo { at, parent, stop: Stop::Broken, frames: None, open: 0 });
        let end = self.walk(at, part, from, t0, id, stack);
        let open = if end.stop.is_clean() { 0 } else { self.open_channels(at, &end.stop) };
        if let Some(run) = self.runs.get_mut(id) {
            run.stop = end.stop;
            run.frames = end.frames;
            run.open = open;
        }
        (id, end.frames)
    }

    /// The channels sound opcodes of script `at` could still change after it stopped as `stop` says.
    fn open_channels(&mut self, at: ScriptRef, stop: &Stop) -> u8 {
        let Some(code) = self.code(at) else { return 0b1111 };
        let (from, to) = match *stop {
            // The loop's body: from the jump's target to the jump itself.
            Stop::IdleLoop { at: jump } => {
                let target = code.index.get(&jump).and_then(|&i| code.instructions.get(i)).and_then(|i| i.jump_target).unwrap_or(0);
                (target, jump.saturating_add(1))
            }
            Stop::Unjudged { at: off } | Stop::Unknown { at: off, .. } => (off, usize::MAX),
            Stop::Budget | Stop::Broken | Stop::NativePartyUnsupported => (0, usize::MAX),
            Stop::Returned | Stop::LeavesField { .. } => return 0,
        };
        let mut mask = 0u8;
        for ins in code.instructions.iter().filter(|i| i.offset >= from && i.offset < to) {
            match Call::of(ins) {
                Ok(Some(call)) => mask |= call.channel_bit(),
                Ok(None) => {}
                // A sound whose channel or id is in a variable could be anything.
                Err(Banked) => mask = 0b1111,
            }
        }
        mask
    }

    fn walk(&mut self, at: ScriptRef, part: Part, from: Option<usize>, t0: u32, run: usize, stack: &mut Vec<ScriptRef>) -> End {
        let broken = End { stop: Stop::Broken, frames: None };
        let Some(code) = self.code(at) else { return broken };
        // Where to start: the given instruction, the start of the script, or the instruction after the first RET.
        let mut pc = match (from, part) {
            (Some(offset), _) => match code.index.get(&offset) {
                Some(&i) => i,
                None => return broken,
            },
            (None, Part::Whole | Part::Init) => 0,
            (None, Part::Main) => match code.first_ret {
                Some(ret) => ret.saturating_add(1),
                None => return End { stop: Stop::Returned, frames: Some(0) },
            },
        };
        let mut t = t0;
        loop {
            self.steps += 1;
            if self.steps > MAX_STEPS || t.saturating_sub(t0) > MAX_FRAMES || self.events.len() >= MAX_EVENTS {
                return End { stop: Stop::Budget, frames: None };
            }
            let took = Some(t.saturating_sub(t0));
            // Falling off the end of a script is what the data does into the RET that follows it.
            let Some(ins) = code.instructions.get(pc) else { return End { stop: Stop::Returned, frames: took } };
            let next = pc.saturating_add(1);
            match ins.opcode {
                OP_RET => return End { stop: Stop::Returned, frames: took },
                OP_JMPF | OP_JMPFL => match code.forward(ins, pc) {
                    Jump::To(i) => pc = i,
                    Jump::End => return End { stop: Stop::Returned, frames: took },
                    Jump::Broken => return broken,
                },
                OP_JMPB | OP_JMPBL => return End { stop: Stop::IdleLoop { at: ins.offset }, frames: None },
                OP_IFUB | OP_IFUBL | OP_IFSW | OP_IFSWL | OP_IFUW | OP_IFUWL | OP_IFKEY | OP_IFKEYON | OP_IFKEYOFF | OP_IFPRTYQ | OP_IFMEMBQ => {
                    let Some(test) = guard_test(ins) else { return broken };
                    match judge(&test, &self.env) {
                        Verdict::True => pc = next,
                        Verdict::False => match code.forward(ins, pc) {
                            Jump::To(i) => pc = i,
                            Jump::End => return End { stop: Stop::Returned, frames: took },
                            Jump::Broken => return broken,
                        },
                        Verdict::Unknown => {
                            self.unjudged.push(Unjudged { by: at, offset: ins.offset, test: ExitGuard { offset: ins.offset, test, negated: false }.to_string() });
                            return End { stop: Stop::Unjudged { at: ins.offset }, frames: None };
                        }
                    }
                }
                OP_WAIT => match ins.literal("frames").and_then(|f| u32::try_from(f).ok()) {
                    Some(f) => {
                        t = t.saturating_add(f);
                        pc = next;
                    }
                    None => return End::unknown(ins),
                },
                OP_REQ | 0x02 | OP_REQEW => {
                    let target = ins.literal("entity").and_then(|e| usize::try_from(e).ok()).zip(ins.literal("script").and_then(|s| u8::try_from(s).ok()));
                    let Some((entity, slot)) = target.filter(|&(e, s)| e < self.script.entity_count() && usize::from(s) < SCRIPTS_PER_ENTITY) else {
                        return End::unknown(ins);
                    };
                    let callee = ScriptRef { entity, slot };
                    if stack.len() > MAX_CALL_DEPTH || stack.contains(&callee) {
                        return End::unknown(ins);
                    }
                    stack.push(callee);
                    let (callee_run, callee_frames) = self.run_inner(callee, Part::Whole, None, t, Some(run), stack);
                    stack.pop();
                    // A callee that leaves the field takes everything with it: nothing after it is run.
                    if let Some(Stop::LeavesField { at: gone }) = self.runs.get(callee_run).map(|r| r.stop) {
                        return End { stop: Stop::LeavesField { at: gone }, frames: took };
                    }
                    if ins.opcode == OP_REQEW {
                        // The caller waits for the callee to end; a callee that does not end in a known time stops it too.
                        match callee_frames {
                            Some(f) => t = t.saturating_add(f),
                            None => return End::unknown(ins),
                        }
                    }
                    pc = next;
                }
                op if leaves(op) => return End { stop: Stop::LeavesField { at: ins.offset }, frames: took },
                _ => {
                    match Call::of(ins) {
                        Ok(Some(call)) => self.events.push(Timed { at: t, call, by: at, offset: ins.offset, run }),
                        Ok(None) => {
                            // Anything not on the instant list, and any write to the story's own variables, ends the run.
                            if !instant(ins.opcode) || writes_story(ins) {
                                return End::unknown(ins);
                            }
                        }
                        Err(Banked) => self.banked += 1,
                    }
                    pc = next;
                }
            }
        }
    }
}
