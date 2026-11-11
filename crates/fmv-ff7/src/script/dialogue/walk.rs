//! The interpreter behind [`super`]: runs one script (and what it requests) until it opens a
//! message window, and again after the player has answered.
//!
//! One instruction is decoded and executed at a time from the field's code area, exactly where the
//! engine would run it: a script that has no terminator falls into the code after it, and a jump
//! may land anywhere an instruction starts (see [`crate::script`] for the decoder's measurements).
//! Everything is bounded by [`Limits`], and nothing here indexes a slice, divides by a script's
//! number or shifts by one without checking it.

use super::pieces::parse_message;
use super::{Beat, Choice, End, Message, MessageOrigin, MessageSource, Piece, StoryVars};
use crate::script::{decode_at, FieldScript, Instruction, INIT_SCRIPT, TALK_SCRIPT};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

#[path = "exact.rs"]
mod exact;
pub use exact::{run_panel_exact, run_script_exact, run_talk_exact, ExactBattle, ExactError, ExactRun, ExactScript, ExactWrite};

/// How far one walk may go. The defaults are far above anything the owned data needs (the longest
/// talk script is a few hundred instructions) and still end a `JMPB 0` wait in a blink.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Instructions executed, in all scripts together.
    pub steps: usize,
    /// Message windows handed out.
    pub beats: usize,
    /// Scripts a script may have requested, one inside the other (a request beyond is skipped).
    pub depth: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self { steps: 20_000, beats: 32, depth: 4 }
    }
}

/// The OK button's bit in a key mask (`IFKEYON 0x20` is "OK pressed"; the ladders and the lift's panel ask for 0x220).
pub const KEY_OK: u32 = 0x20;

/// Most movements a walk keeps in its journal (a script that moves a character more often than this keeps the first).
const MAX_MOVES: usize = 64;

/// Where the movement opcodes have left a character: the end of the movement, with what the opcode said of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spot {
    pub x: i32,
    pub y: i32,
    /// The height, when the opcode says it (`XYZI`, `XYZ`, `LADER`); `None` after one that leaves it to the engine
    /// (`MOVE`, `JUMP`, `XYI`).
    pub z: Option<i32>,
    /// The walkmesh triangle the place is on, when the opcode says it (`XYZI`, `XYI`, `JUMP`, `LADER`).
    pub triangle: Option<u16>,
    /// Which opcode it was (the mnemonic of the last one that moved the character).
    pub by: &'static str,
}

/// A script being run: where it is in each script it requested.
#[derive(Debug, Clone)]
struct Frame {
    /// Section-relative offset of the next instruction.
    pc: usize,
    /// The entity whose script this is (`RETTO` stays in it).
    entity: usize,
    /// For a script that was requested without waiting for its end (`REQ`, `REQSW`): the
    /// instructions it has run. The engine runs such a script alongside its requester, and many of
    /// them idle in a loop waiting for an event; here it stops at the first instruction it meets
    /// twice. `None` for the entry script and for `REQEW`, which really is waited for.
    seen: Option<BTreeSet<usize>>,
}

impl Frame {
    fn new(pc: usize, entity: usize) -> Self {
        Self { pc, entity, seen: None }
    }
}

/// An `ASK` waiting for its answer.
#[derive(Debug, Clone, Copy)]
struct Pending {
    var: Option<(u8, u8)>,
    first: usize,
    last: usize,
    cursor: usize,
}

/// What one instruction did.
enum Flow {
    Continue,
    Show(Message),
    Ask(Message, Pending),
    End(End),
}

/// The width an operand is read at.
#[derive(Debug, Clone, Copy)]
enum Width {
    U8,
    S16,
    U16,
}

/// What the arithmetic opcodes do to their variable.
#[derive(Debug, Clone, Copy)]
enum Arith {
    Set,
    Plus,
    Minus,
    Mul,
    Div,
    Mod,
    And,
    Or,
    Xor,
    Inc,
    Dec,
}

/// `(operation, word, saturating)` of an arithmetic opcode.
fn arith_of(opcode: u8) -> Option<(Arith, bool, bool)> {
    Some(match opcode {
        0x76 => (Arith::Plus, false, true),
        0x77 => (Arith::Plus, true, true),
        0x78 => (Arith::Minus, false, true),
        0x79 => (Arith::Minus, true, true),
        0x7A => (Arith::Inc, false, true),
        0x7B => (Arith::Inc, true, true),
        0x7C => (Arith::Dec, false, true),
        0x7D => (Arith::Dec, true, true),
        0x80 => (Arith::Set, false, false),
        0x81 => (Arith::Set, true, false),
        0x85 => (Arith::Plus, false, false),
        0x86 => (Arith::Plus, true, false),
        0x87 => (Arith::Minus, false, false),
        0x88 => (Arith::Minus, true, false),
        0x89 => (Arith::Mul, false, false),
        0x8A => (Arith::Mul, true, false),
        0x8B => (Arith::Div, false, false),
        0x8C => (Arith::Div, true, false),
        0x8D => (Arith::Mod, false, false),
        0x8E => (Arith::Mod, true, false),
        0x8F => (Arith::And, false, false),
        0x90 => (Arith::And, true, false),
        0x91 => (Arith::Or, false, false),
        0x92 => (Arith::Or, true, false),
        0x93 => (Arith::Xor, false, false),
        0x94 => (Arith::Xor, true, false),
        0x95 => (Arith::Inc, false, false),
        0x96 => (Arith::Inc, true, false),
        0x97 => (Arith::Dec, false, false),
        0x98 => (Arith::Dec, true, false),
        _ => return None,
    })
}

/// Whether bit `bit` of `value` is set (`false` for a bit the operand's width does not have).
fn bit_set(value: i32, bit: i32, bits: u32) -> bool {
    u32::try_from(bit).ok().filter(|&b| b < bits).and_then(|b| value.checked_shr(b)).is_some_and(|v| v & 1 == 1)
}

/// The eleven operators of the `IF*` opcodes (FFRTT, `14_IFUB`): `==`, `!=`, `>`, `<`, `>=`, `<=`,
/// the three bitwise ones (true when the result is not 0), "bit on" and "bit off". Anything else
/// is false.
fn compare(op: i32, left: i32, right: i32, bits: u32) -> bool {
    match op {
        0 => left == right,
        1 => left != right,
        2 => left > right,
        3 => left < right,
        4 => left >= right,
        5 => left <= right,
        6 => left & right != 0,
        7 => left ^ right != 0,
        8 => left | right != 0,
        9 => bit_set(left, right, bits),
        10 => !bit_set(left, right, bits),
        _ => false,
    }
}

/// Number of lines on the last page of `pieces`: the count an `ASK`'s line numbers refer to.
fn last_page_lines(pieces: &[Piece]) -> usize {
    let mut lines = 1usize;
    for piece in pieces {
        match piece {
            Piece::NewPage => lines = 1,
            Piece::NewLine => lines = lines.saturating_add(1),
            _ => {}
        }
    }
    lines
}

/// A walk through one script. Make one with [`Walk::script`] or [`Walk::talk`] and call
/// [`Walk::resume`] with the field's script for each window.
#[derive(Debug, Clone)]
pub struct Walk {
    entity: usize,
    slot: usize,
    limits: Limits,
    started: bool,
    frames: Vec<Frame>,
    /// Variables the walk has written (`(bank, address)` to byte); they shadow the story hook.
    writes: BTreeMap<(u8, u8), u8>,
    steps: usize,
    beats: usize,
    rng: u32,
    pending: Option<Pending>,
    done: Option<End>,
    /// The instructions run since anything last changed (a variable written, a window opened, a
    /// script entered or left): meeting one again means nothing can ever differ, a loop that
    /// idles for ever.
    idle: BTreeSet<usize>,
    /// The one-pass prelude of [`availability`]: no windows, loops cut short.
    quiet: bool,
    passed_init: bool,
    visited: BTreeSet<usize>,
    talk_off: bool,
    visible: bool,
    /// A gift opcode (`STITM` item, `SMTRA` materia, `GOLDu` gil) ran and this walk does not
    /// perform it: the next window the script shows (the game's own "Received ..." line) is
    /// followed by an added note that nothing was given, so the canon line never misleads.
    gift_not_given: bool,
    /// The added note, waiting to be shown after the canon window that announced the gift.
    queued: Option<Message>,
    /// The keys the player presses, as a bit mask ([`KEY_OK`]): what `IFKEY` and `IFKEYON` test. 0: none.
    pressed: u32,
    /// The walk follows the movement opcodes (see [`Walk::tracking_moves`]); off, they are ignored as they always were.
    track: bool,
    /// The first `RET` of the entity's script 0 carries the walk on into its main part instead of ending it (see
    /// [`Walk::through_init`]).
    through_init: bool,
    /// Where the movement opcodes have left each entity, by entity.
    spots: BTreeMap<usize, Spot>,
    /// The movements in the order they happened: the entity and where it was left (at most [`MAX_MOVES`]).
    moves: Vec<(usize, Spot)>,
    /// The battle a `BATTLE` started, if one did.
    battle: Option<u16>,
    /// A read-back (`AXYZI`) could not fill a variable because the walk did not know the place's height.
    unresolved: bool,
    /// Opt-in authoritative evaluation; legacy dialogue remains deliberately permissive.
    exact: Option<exact::ExactState>,
}

/// The layer of the notes this project adds when a script's effect is not performed.
pub const EFFECT_NOTE_LAYER: &str = "not-performed";

/// The note shown after a canon line that announced a gift this walk does not give.
fn gift_note() -> Message {
    Message::new(
        MessageOrigin::Added { layer: EFFECT_NOTE_LAYER.to_string() },
        vec![Piece::Text("(Nothing was given: picking up items from".to_string()), Piece::NewLine, Piece::Text("characters is not in the game yet.)".to_string())],
    )
}

impl Walk {
    /// A walk through script `slot` of `entity`, which starts at the first [`Walk::resume`].
    /// `seed` feeds `RANDOM` (the same seed gives the same walk).
    pub fn script(entity: usize, slot: usize, seed: u32, limits: Limits) -> Self {
        Self {
            entity,
            slot,
            limits,
            started: false,
            frames: Vec::new(),
            writes: BTreeMap::new(),
            steps: 0,
            beats: 0,
            // Spread the seed over the whole word (a small one would give a run of small numbers).
            rng: seed.wrapping_mul(0x9E37_79B9) | 1,
            pending: None,
            done: None,
            idle: BTreeSet::new(),
            quiet: false,
            passed_init: false,
            visited: BTreeSet::new(),
            talk_off: false,
            visible: true,
            gift_not_given: false,
            queued: None,
            pressed: 0,
            track: false,
            through_init: false,
            spots: BTreeMap::new(),
            moves: Vec::new(),
            battle: None,
            unresolved: false,
            exact: None,
        }
    }

    /// The same walk, with the player pressing the keys of `keys` (a bit mask, [`KEY_OK`] for the OK button) for
    /// as long as it runs: `IFKEY` and `IFKEYON` answer true for a mask that shares a bit with it (`IFKEYOFF`
    /// never does). Without this no key is ever held.
    #[must_use]
    pub fn pressing(mut self, keys: u32) -> Self {
        self.pressed = keys;
        self
    }

    /// The same walk, running an entity's script 0 as the engine does: the init part and then, after its first `RET`,
    /// the main part (a loop that waits for something: a panel that waits for OK). A loop that has nothing more to do
    /// (the press was used up, nothing changed) ends the walk as any idle loop does.
    #[must_use]
    pub fn through_init(mut self) -> Self {
        self.through_init = true;
        self
    }

    /// The same walk, following the movement opcodes (`XYZI`, `XYI`, `XYZ`, `MOVE`, `JUMP`, `LADER`) and the read-backs
    /// (`GETAI`, `AXYZI`). Without this they are ignored, which is what a conversation wants: a character that walks about
    /// in a loop must not keep a walk from seeing that nothing else changes.
    #[must_use]
    pub fn tracking_moves(mut self) -> Self {
        self.track = true;
        self
    }

    /// Where the walk has left `entity` by a movement opcode (`None`: it has not moved it).
    pub fn spot(&self, entity: usize) -> Option<Spot> {
        self.spots.get(&entity).copied()
    }

    /// The movements the walk made, in order: the entity and the place each was left at.
    pub fn moves(&self) -> &[(usize, Spot)] {
        &self.moves
    }

    /// The battle the script started (`BATTLE`), if it got that far.
    pub fn battle(&self) -> Option<u16> {
        self.battle
    }

    /// A read-back of a character's place could not fill its height variable (the walk did not know the height and
    /// [`StoryVars::height`] did not either): whatever the script decided from it was decided on a guess.
    pub fn unresolved(&self) -> bool {
        self.unresolved
    }

    /// A walk through the talk script (slot 1) of `entity`.
    pub fn talk(entity: usize, seed: u32, limits: Limits) -> Self {
        Self::script(entity, TALK_SCRIPT, seed, limits)
    }

    /// Start from variables already written (the prelude's).
    fn with_writes(mut self, writes: BTreeMap<(u8, u8), u8>) -> Self {
        self.writes = writes;
        self
    }

    /// Instructions executed so far.
    pub fn steps(&self) -> usize {
        self.steps
    }

    /// Windows handed out so far.
    pub fn beats(&self) -> usize {
        self.beats
    }

    /// The value the walk has written to a variable, if it has.
    pub fn written(&self, bank: u8, address: u8) -> Option<u8> {
        self.writes.get(&(bank, address)).copied()
    }

    /// Run on to the next window or the end. `answer` is the player's answer to the window this
    /// walk handed out last, if that asked (see [`MessageSource::next`]).
    pub fn resume(&mut self, script: &FieldScript<'_>, env: &dyn StoryVars, answer: Option<usize>) -> Beat {
        if self.exact.is_none() && !env.execution_supported() {
            self.queued = None;
            self.pending = None;
            return self.finish(End::Broken);
        }
        if let Some(note) = self.queued.take() {
            return Beat::Message(note);
        }
        if let Some(end) = self.done {
            return Beat::End(end);
        }
        if let Some(p) = self.pending.take() {
            if self.exact.is_some() {
                match answer {
                    None => self.fail_exact(ExactError::MissingAnswer),
                    Some(answer) if !(p.first..=p.last).contains(&answer) =>
                        self.fail_exact(ExactError::InvalidAnswer { answer, first: p.first, last: p.last }),
                    _ => {}
                }
            }
            let chosen = answer.map_or(p.cursor, |a| a.clamp(p.first, p.last.max(p.first)));
            if let Some((bank, address)) = p.var {
                self.set_byte(bank, address, u8::try_from(chosen).unwrap_or(u8::MAX));
            }
        }
        if !self.started {
            self.started = true;
            match script.span(self.entity, self.slot) {
                Some(span) => self.frames.push(Frame::new(span.start, self.entity)),
                None => return self.finish(End::Finished),
            }
        }
        loop {
            let flow = self.step(script, env);
            if self.exact_fault().is_some() { return self.finish(End::Broken); }
            match flow {
                Flow::Continue => {}
                Flow::Show(message) => {
                    if std::mem::take(&mut self.gift_not_given) && !self.quiet {
                        self.queued = Some(gift_note());
                    }
                    return Beat::Message(message);
                }
                Flow::Ask(message, pending) => {
                    self.pending = Some(pending);
                    return Beat::Message(message);
                }
                Flow::End(end) => return self.finish(end),
            }
        }
    }

    fn finish(&mut self, end: End) -> Beat {
        self.done = Some(end);
        Beat::End(end)
    }

    // ---- variables --------------------------------------------------------------------------

    fn var_byte(&self, env: &dyn StoryVars, bank: u8, address: u8) -> u8 {
        self.read_masked(env, bank, address, 255)
    }

    fn var_word(&self, env: &dyn StoryVars, bank: u8, address: u8) -> u16 {
        if address == u8::MAX { self.invalid_exact(); }
        let low = self.var_byte(env, bank, address);
        let high = address.checked_add(1).map_or(0, |a| self.var_byte(env, bank, a));
        u16::from_le_bytes([low, high])
    }

    fn set_byte(&mut self, bank: u8, address: u8, value: u8) {
        self.set_byte_with_mask(bank, address, value, 255);
    }

    fn set_word(&mut self, bank: u8, address: u8, value: u16) {
        if self.exact.is_some() && address == u8::MAX { self.invalid_exact(); return; }
        if self.native_word(bank, address, value) { return; }
        let [low, high] = value.to_le_bytes();
        self.set_byte(bank, address, low);
        if let Some(next) = address.checked_add(1) {
            self.set_byte(bank, next, high);
        }
    }

    /// The value of operand `name`: the literal, or the variable it names read at `width`; 0 when
    /// the instruction has no such operand.
    fn num(&self, env: &dyn StoryVars, ins: &Instruction, name: &str, width: Width) -> i32 {
        let Some(op) = ins.operand(name) else { return 0 };
        if op.bank == 0 {
            return op.value;
        }
        let address = u8::try_from(op.value & 0xFF).unwrap_or(0);
        match width {
            Width::U8 => i32::from(self.var_byte(env, op.bank, address)),
            Width::U16 => i32::from(self.var_word(env, op.bank, address)),
            Width::S16 => i32::from(i16::from_le_bytes(self.var_word(env, op.bank, address).to_le_bytes())),
        }
    }

    /// `(bank, address)` of the variable operand `name` (`None` for a literal: nothing to write).
    fn dest(ins: &Instruction, name: &str) -> Option<(u8, u8)> {
        let op = ins.operand(name)?;
        (op.bank != 0).then(|| (op.bank, u8::try_from(op.value & 0xFF).unwrap_or(0)))
    }

    fn store(&mut self, (bank, address): (u8, u8), wide: bool, value: i64, saturate: bool) {
        if wide {
            let v = if saturate { value.clamp(0, 0xFFFF) } else { value.rem_euclid(0x1_0000) };
            self.set_word(bank, address, u16::try_from(v).unwrap_or(0));
        } else {
            let v = if saturate { value.clamp(0, 0xFF) } else { value.rem_euclid(0x100) };
            self.set_byte(bank, address, u8::try_from(v).unwrap_or(0));
        }
    }

    fn arithmetic(&mut self, env: &dyn StoryVars, ins: &Instruction, op: Arith, wide: bool, saturate: bool) {
        let Some(var) = Self::dest(ins, "var") else { self.invalid_exact(); return };
        // (a plain set never looks at what the variable held)
        let cur = if matches!(op, Arith::Set) {
            0
        } else if wide {
            i64::from(self.var_word(env, var.0, var.1))
        } else {
            i64::from(self.var_byte(env, var.0, var.1))
        };
        let value = i64::from(self.num(env, ins, "value", if wide { Width::S16 } else { Width::U8 }));
        let new = match op {
            Arith::Set => value,
            Arith::Plus => cur.saturating_add(value),
            Arith::Minus => cur.saturating_sub(value),
            Arith::Mul => cur.saturating_mul(value),
            Arith::Div => match cur.checked_div(value) {
                Some(q) => q,
                None => { self.invalid_exact(); return },
            },
            Arith::Mod => match cur.checked_rem(value) {
                Some(r) => r,
                None => { self.invalid_exact(); return },
            },
            Arith::And => cur & value,
            Arith::Or => cur | value,
            Arith::Xor => cur ^ value,
            Arith::Inc => cur.saturating_add(1),
            Arith::Dec => cur.saturating_sub(1),
        };
        self.store(var, wide, new, saturate);
    }

    /// `BITON`/`BITOFF`/`BITXOR`: the variable is a row of bits, bit `n` being bit `n % 8` of the
    /// byte `n / 8` after its address (so bits 8..16 are the second byte of a word, as the word
    /// compare reads them).
    fn bit_op(&mut self, env: &dyn StoryVars, ins: &Instruction, op: u8) {
        let Some((bank, address)) = Self::dest(ins, "var") else { self.invalid_exact(); return };
        let bit = self.num(env, ins, "bit", Width::U8).rem_euclid(256);
        // Original BITON/OFF/XOR writes one byte. The legacy preview's cross-byte
        // interpretation is not evidence for an authoritative write beyond bit seven.
        if self.exact.is_some() && bit >= 8 { self.invalid_exact(); return; }
        let byte_at = address.checked_add(u8::try_from(bit / 8).unwrap_or(0));
        let (Some(byte_at), Some(mask)) = (byte_at, 1u8.checked_shl(u32::try_from(bit % 8).unwrap_or(0))) else { self.invalid_exact(); return };
        let cur = if self.exact.is_some() {
            if op == 0x84 { self.read_masked(env, bank, byte_at, mask) } else { self.peek_byte(env, bank, byte_at) }
        } else { self.var_byte(env, bank, byte_at) };
        let new = match op {
            0x82 => cur | mask,
            0x83 => cur & !mask,
            _ => cur ^ mask,
        };
        if self.exact.is_some() { self.set_masked(env, bank, byte_at, new, mask); }
        else { self.set_byte(bank, byte_at, new); }
    }

    /// `XYZI`, `XYI`, `XYZ`, `MOVE`, `JUMP` and `LADER`: `entity` is left where the instruction says (the movement
    /// is not timed or animated here: it is its end).
    fn moved(&mut self, env: &dyn StoryVars, entity: usize, ins: &Instruction) {
        let has = |name: &str| ins.operand(name).is_some();
        let x = self.num(env, ins, "x", Width::S16);
        let y = self.num(env, ins, "y", Width::S16);
        let z = has("z").then(|| self.num(env, ins, "z", Width::S16));
        let triangle = if has("triangle") { u16::try_from(self.num(env, ins, "triangle", Width::U16)).ok() } else { None };
        let spot = Spot { x, y, z, triangle, by: ins.mnemonic };
        if self.moves.len() < MAX_MOVES {
            self.moves.push((entity, spot));
        }
        // A loop that moves a character to the same place every lap changes nothing, like any other idle loop.
        if self.spots.insert(entity, spot) != Some(spot) {
            self.idle.clear();
        }
    }

    /// `GETAI` (the triangle) and `AXYZI` (the place) of an entity, into the variables the instruction names, as
    /// words. Only what the walk knows is written; a read-back it cannot fill (the entity was never moved by the walk,
    /// or its height is not known) leaves the variable alone and marks the walk [`Walk::unresolved`].
    fn read_back(&mut self, env: &dyn StoryVars, ins: &Instruction) {
        let Some(entity) = ins.literal("entity").and_then(|e| usize::try_from(e).ok()) else { return };
        let Some(spot) = self.spots.get(&entity).copied() else {
            self.unresolved = true;
            return;
        };
        let word = |v: i32| u16::from_le_bytes(i16::try_from(v).unwrap_or(0).to_le_bytes());
        if let (Some((bank, address)), Some(triangle)) = (Self::dest(ins, "var_triangle"), spot.triangle) {
            self.set_word(bank, address, triangle);
        } else if ins.operand("var_triangle").is_some_and(|o| o.bank != 0) {
            self.unresolved = true;
        }
        if ins.mnemonic != "AXYZI" {
            return;
        }
        if let Some((bank, address)) = Self::dest(ins, "var_x") {
            self.set_word(bank, address, word(spot.x));
        }
        if let Some((bank, address)) = Self::dest(ins, "var_y") {
            self.set_word(bank, address, word(spot.y));
        }
        if let Some((bank, address)) = Self::dest(ins, "var_z") {
            match spot.z.or_else(|| env.height(spot.x, spot.y, spot.triangle)) {
                Some(z) => self.set_word(bank, address, word(z)),
                None => self.unresolved = true,
            }
        }
    }

    fn random(&mut self) -> u8 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        u8::try_from(x >> 24).unwrap_or(0)
    }

    // ---- messages ---------------------------------------------------------------------------

    /// The message of dialog `dialog` as `entity`'s script shows it.
    fn message(&self, script: &FieldScript<'_>, env: &dyn StoryVars, entity: usize, window: i32, dialog: i32) -> Option<Message> {
        let index = usize::try_from(dialog).ok()?;
        let d = script.dialogs().get(index)?;
        let raw = script.bytes().get(d.offset..d.offset.checked_add(d.len)?)?;
        let pieces = if self.native_exact() { self.exact_message(raw, env) } else { parse_message(raw, env) };
        let mut message = Message::new(MessageOrigin::Canon { entity, dialog: index }, pieces);
        message.window = u8::try_from(window).unwrap_or(0);
        Some(message)
    }

    fn open_window(&mut self) -> Result<(), End> {
        if self.beats >= self.limits.beats {
            return Err(End::Budget);
        }
        self.beats = self.beats.saturating_add(1);
        self.idle.clear();
        Ok(())
    }

    fn ask(&mut self, script: &FieldScript<'_>, env: &dyn StoryVars, entity: usize, ins: &Instruction) -> Flow {
        let (Some(window), Some(dialog), Some(first), Some(last)) =
            (ins.literal("window"), ins.literal("dialog"), ins.literal("first"), ins.literal("last"))
        else {
            self.invalid_exact();
            return Flow::Continue;
        };
        let Some(mut message) = self.message(script, env, entity, window, dialog) else { self.invalid_exact(); return Flow::Continue };
        let var = Self::dest(ins, "var");
        let first = usize::try_from(first).unwrap_or(0);
        // A last line the text does not have (20 of the 908 `ASK`s) is the text's last line.
        let lines = last_page_lines(&message.pieces);
        if self.exact.is_some() {
            if self.quiet { self.fail_exact(ExactError::MissingAnswer); return Flow::End(End::Broken); }
            if var.is_none() || usize::try_from(last).ok().is_none_or(|last| last < first || last >= lines) {
                self.invalid_exact();
                return Flow::End(End::Broken);
            }
        }
        let last = usize::try_from(last).unwrap_or(0).max(first).min(lines.saturating_sub(1));
        // An explicit answer does not depend on a possibly unknown previous cursor value.
        let held = if self.exact.is_some() { None } else { var.map(|(bank, address)| usize::from(self.var_byte(env, bank, address))) };
        let cursor = held.filter(|c| (first..=last).contains(c)).unwrap_or(first);
        if let Some((bank, address)) = var {
            // The variable always ends up holding an answer, even when nobody was asked.
            if self.quiet || first > last {
                self.set_byte(bank, address, u8::try_from(cursor).unwrap_or(u8::MAX));
            }
        }
        if self.quiet {
            return Flow::Continue;
        }
        if let Err(end) = self.open_window() {
            return Flow::End(end);
        }
        if first > last {
            // No choice line in the text: a plain window.
            return Flow::Show(message);
        }
        message.choice = Some(Choice { first, last, cursor });
        Flow::Ask(message, Pending { var, first, last, cursor })
    }

    // ---- one instruction --------------------------------------------------------------------

    fn set_pc(&mut self, pc: usize) {
        if let Some(frame) = self.frames.last_mut() {
            frame.pc = pc;
        }
    }

    /// Leave the instruction's own script: back to whoever requested it, or the end.
    fn ret(&mut self) -> Flow {
        self.frames.pop();
        self.idle.clear();
        if self.frames.is_empty() {
            Flow::End(End::Finished)
        } else {
            Flow::Continue
        }
    }

    fn jump(&mut self, ins: &Instruction) -> Flow {
        match ins.jump_target {
            Some(target) => {
                self.set_pc(target);
                Flow::Continue
            }
            None => Flow::End(End::Broken),
        }
    }

    /// Continue after a conditional: into its body when `holds`, else past it.
    fn branch(&mut self, ins: &Instruction, holds: bool) -> Flow {
        if holds {
            Flow::Continue
        } else {
            self.jump(ins)
        }
    }

    fn step(&mut self, script: &FieldScript<'_>, env: &dyn StoryVars) -> Flow {
        if self.steps >= self.limits.steps {
            return Flow::End(End::Budget);
        }
        self.steps = self.steps.saturating_add(1);
        let Some(top) = self.frames.last_mut() else { return Flow::End(End::Finished) };
        let (pc, entity) = (top.pc, top.entity);
        if top.seen.as_mut().is_some_and(|seen| !seen.insert(pc)) {
            // A script requested without waiting went round a loop: leave it.
            return self.ret();
        }
        let idle = !self.idle.insert(pc);
        if idle && self.exact.is_none() {
            // A permissive preview ends at its unchanged idle loop. Exact mode defers this
            // decision until a potentially stale actor observation has been checked below.
            return Flow::End(End::Finished);
        }
        let frame = Frame::new(pc, entity);
        if self.quiet && !self.visited.insert(frame.pc) {
            // The prelude is one pass: an instruction met twice is a loop (the idle `JMPB 0`) in
            // the entity's own script, or a script requested a second time (skipped).
            return if self.frames.len() > 1 { self.ret() } else { Flow::End(End::Finished) };
        }
        let code_end = script.layout().code_end;
        if frame.pc >= code_end {
            // Ran off the end of the code: as good as a RET.
            if self.exact.is_some() { return Flow::End(End::Broken); }
            return self.ret();
        }
        let Some(code) = script.bytes().get(..code_end) else { return Flow::End(End::Broken) };
        let Ok(ins) = decode_at(code, frame.pc, 0) else { return Flow::End(End::Broken) };
        if self.exact_completed_idle(script, env, &ins) { return Flow::End(End::Finished); }
        if idle && !self.exact_tainted_getai(&ins) { return Flow::End(End::Finished); }
        self.exact_instruction(&ins, frame.entity);
        if self.exact_fault().is_some() { return Flow::End(End::Broken); }
        self.set_pc(ins.end());
        let op = ins.opcode;
        match op {
            0x00 => {
                if (self.quiet || self.through_init) && !self.passed_init && self.frames.len() == 1 {
                    // The main part of script 0 follows the first RET.
                    self.passed_init = true;
                    return Flow::Continue;
                }
                self.ret()
            }
            // REQ, REQSW, REQEW: the requested script runs first, to its end (to its first loop for
            // the two that do not wait for the end), and then the requester goes on.
            0x01..=0x03 => {
                if self.exact.is_some() && op != 0x03 && !self.exact_joined_request(script, env, &ins)
                    && !self.exact_presentation_request(script, &ins) {
                    self.fail_exact(ExactError::Request { offset: ins.offset });
                    return Flow::End(End::Broken);
                }
                if self.frames.len() > self.limits.depth {
                    self.fail_exact(ExactError::Request { offset: ins.offset });
                    return Flow::Continue;
                }
                let target = ins.literal("entity").zip(ins.literal("script")).and_then(|(e, s)| {
                    let (entity, slot) = (usize::try_from(e).ok()?, usize::try_from(s).ok()?);
                    Some((entity, script.span(entity, slot)?))
                });
                if let Some((entity, span)) = target {
                    let mut callee = Frame::new(span.start, entity);
                    if op != 0x03 && self.exact.is_none() {
                        callee.seen = Some(BTreeSet::new());
                    }
                    self.frames.push(callee);
                    self.idle.clear();
                } else {
                    self.fail_exact(ExactError::Request { offset: ins.offset });
                }
                Flow::Continue
            }
            // RETTO: carry on in another script of the same entity, in place of this one.
            0x07 => {
                let slot = ins.literal("script").and_then(|s| usize::try_from(s).ok());
                match slot.and_then(|s| script.span(frame.entity, s)) {
                    Some(span) => {
                        self.set_pc(span.start);
                        Flow::Continue
                    }
                    None => { self.fail_exact(ExactError::Request { offset: ins.offset }); self.ret() },
                }
            }
            0x10..=0x13 => self.jump(&ins),
            0x14..=0x19 => {
                let (width, bits) = match op {
                    0x14 | 0x15 => (Width::U8, 8),
                    0x16 | 0x17 => (Width::S16, 16),
                    _ => (Width::U16, 16),
                };
                let comparison = ins.literal("op").unwrap_or(-1);
                if !(0..=10).contains(&comparison) { self.invalid_exact(); }
                let right = self.num(env, &ins, "right", width);
                let selected_bit = if self.exact.is_some() && matches!(comparison, 9 | 10) {
                    Self::dest(&ins, "left").and_then(|(bank, address)| {
                        let bit = u8::try_from(right).ok().filter(|&bit| u32::from(bit) < bits)?;
                        let address = address.checked_add(bit / 8)?;
                        Some(self.read_masked(env, bank, address, 1 << (bit % 8)) & (1 << (bit % 8)) != 0)
                    })
                } else { None };
                let holds = selected_bit.map(|on| if comparison == 9 { on } else { !on })
                    .unwrap_or_else(|| compare(comparison, self.num(env, &ins, "left", width), right, bits));
                self.branch(&ins, holds)
            }
            // The keys the walk presses (none unless asked to: `Walk::pressing`): `IFKEY` and `IFKEYON` hold when one
            // of the keys they ask for is among them, `IFKEYOFF` (a key let go of) never does.
            0x30..=0x32 => {
                let asked = ins.literal("keys").and_then(|k| u32::try_from(k).ok()).unwrap_or(0);
                let holds = op != 0x32 && asked & self.pressed != 0;
                // `IFKEYON` is the frame the key goes down: the press is used up by the first test that sees it (a loop
                // that polls the key runs its body once for one press)
                if holds && op == 0x31 {
                    self.pressed &= !asked;
                    self.idle.clear();
                }
                self.branch(&ins, holds)
            }
            0x20 => Flow::End(End::MiniGame),
            0x38 if self.exact.is_some() => { self.exact_countdown(&ins); Flow::Continue },
            0x40 => {
                let (Some(window), Some(dialog)) = (ins.literal("window"), ins.literal("dialog")) else { self.invalid_exact(); return Flow::Continue };
                let Some(message) = self.message(script, env, frame.entity, window, dialog) else { self.invalid_exact(); return Flow::Continue };
                if self.quiet {
                    return Flow::Continue;
                }
                match self.open_window() {
                    Ok(()) => Flow::Show(message),
                    Err(end) => Flow::End(end),
                }
            }
            0x48 => self.ask(script, env, frame.entity, &ins),
            // STITM / SMTRA / GOLDu give the player something: not performed here (the server owns
            // the inventory), so the announcement that follows gets an added note.
            0x58 | 0x5B | 0x39 => {
                self.gift_not_given = true;
                Flow::Continue
            }
            0x60 => Flow::End(End::MapJump { field: ins.literal("field").and_then(|f| u16::try_from(f).ok()).unwrap_or(0) }),
            0x70 => {
                self.battle = u16::try_from(self.num(env, &ins, "battle", Width::U16)).ok();
                if self.exact.is_some() && self.battle.is_some_and(|formation| self.exact_battle(formation)) {
                    return Flow::Continue;
                }
                Flow::End(End::Battle)
            }
            0x72 if self.exact.is_some() => { self.exact_battle_mode(&ins); Flow::Continue },
            0xBF if self.exact.is_some() => { self.exact_control(&ins); Flow::Continue },
            0x0A | 0x0B | 0xC8..=0xCA | 0xCD..=0xCF if self.native_exact() => {
                self.native_party_instruction(env, &ins); Flow::Continue
            }
            0xCA if self.exact.is_some() => { self.exact_party(&ins); Flow::Continue },
            // The movement opcodes: a character is left where the movement ends.
            0xA5..=0xA8 | 0xC0 | 0xC2 if self.track => {
                self.moved(env, frame.entity, &ins);
                Flow::Continue
            }
            // GETAI and AXYZI read a character's place back into variables.
            0xB9 if self.exact.is_some() => {
                self.exact_getai(&ins);
                Flow::Continue
            }
            0xB9 | 0xC1 if self.track => {
                self.read_back(env, &ins);
                Flow::Continue
            }
            0x74 => {
                // GETPC: the character in a party slot (0xFF for none).
                let slot = ins.literal("party").and_then(|s| u8::try_from(s).ok()).unwrap_or(u8::MAX);
                // Pinned field_opcodes.c:6683–6694 performs no write for slot >= NUM_PARTY.
                if self.native_exact() && slot >= 3 { return Flow::Continue; }
                if let Some((bank, address)) = Self::dest(&ins, "var_character") {
                    self.set_byte(bank, address, self.party_member(env, slot).unwrap_or(u8::MAX));
                } else if self.native_exact() {
                    self.invalid_exact();
                }
                Flow::Continue
            }
            0x7E => {
                if self.exact.is_none() || frame.entity == self.entity {
                    if let Some(v) = ins.literal("disabled") { self.talk_off = v != 0; }
                }
                Flow::Continue
            }
            0x82..=0x84 => {
                self.bit_op(env, &ins, op);
                Flow::Continue
            }
            0x99 => {
                if let Some((bank, address)) = Self::dest(&ins, "var") {
                    let value = self.random();
                    self.set_byte(bank, address, value);
                }
                Flow::Continue
            }
            // LBYTE and HBYTE set the low and the high byte of a word variable.
            0x9A | 0x9B => {
                if let Some((bank, address)) = Self::dest(&ins, "var") {
                    if self.exact.is_some() {
                        // Original OpcodeFuncHbyte extracts the high source byte into the
                        // stated destination; it does not increment the destination address.
                        if let Some(value) = self.exact_source_byte(env, &ins, op == 0x9B) {
                            self.set_byte(bank, address, value);
                        }
                        return Flow::Continue;
                    }
                    let value = u8::try_from(self.num(env, &ins, "value", Width::U16) & 0xFF).unwrap_or(0);
                    let at = if op == 0x9A { Some(address) } else { address.checked_add(1) };
                    if let Some(at) = at {
                        self.set_byte(bank, at, value);
                    }
                } else {
                    self.invalid_exact();
                }
                Flow::Continue
            }
            0x9C => {
                if let Some((bank, address)) = Self::dest(&ins, "var") {
                    let low = u8::try_from(self.num(env, &ins, "low", Width::U8) & 0xFF).unwrap_or(0);
                    let high = u8::try_from(self.num(env, &ins, "high", Width::U8) & 0xFF).unwrap_or(0);
                    self.set_word(bank, address, u16::from_le_bytes([low, high]));
                }
                Flow::Continue
            }
            0xA4 => {
                if self.exact.is_none() || frame.entity == self.entity {
                    if let Some(v) = ins.literal("visible") { self.visible = v != 0; }
                }
                Flow::Continue
            }
            0xCB | 0xCC => {
                let holds = ins.literal("character").and_then(|c| u8::try_from(c).ok()).is_some_and(|c|
                    if op == 0xCB { self.in_party(env, c) }
                    else if self.native_exact() { self.native_member(c) }
                    else { env.is_member(c) });
                self.branch(&ins, holds)
            }
            0xFF => Flow::End(End::GameOver),
            other => {
                if let Some((arith, wide, saturate)) = arith_of(other) {
                    self.arithmetic(env, &ins, arith, wide, saturate);
                }
                Flow::Continue
            }
        }
    }
}

/// What the start-up passes of a character's script 0 leave of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Availability {
    /// Nothing has switched talking off (`TLKON 1`) by the end of the passes.
    pub talkable: bool,
    /// Nothing has hidden the character (`VISI 0`) by the end of the passes.
    pub visible: bool,
}

/// Run the init part and then the main part of `entity`'s script 0 once each (a loop is cut at its
/// second lap), with every window ignored, and report whether the character can be talked to:
/// the field's own set-up decides that with story variables (a character that stands there until
/// the story moves on carries a `TLKON 1` behind a progress test). Returns the variables the
/// passes wrote too, which the talk that follows starts from.
fn prelude(script: &FieldScript<'_>, env: &dyn StoryVars, entity: usize, limits: Limits) -> (Availability, BTreeMap<(u8, u8), u8>) {
    if !env.execution_supported() {
        return (Availability { talkable: false, visible: false }, BTreeMap::new());
    }
    let mut walk = Walk::script(entity, INIT_SCRIPT, 1, limits);
    walk.quiet = true;
    // A quiet walk never hands out a window: it only ever ends.
    while !matches!(walk.resume(script, env, None), Beat::End(_)) {}
    (Availability { talkable: !walk.talk_off, visible: walk.visible }, walk.writes)
}

/// [`prelude`] for callers that only want the verdict.
pub fn availability(script: &FieldScript<'_>, entity: usize, env: &dyn StoryVars) -> Availability {
    prelude(script, env, entity, Limits::default()).0
}

/// What a whole conversation did to the variables, see [`run_talk`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TalkRun {
    /// The variables the talk script wrote, as `(bank, address)` to byte, besides what the character's own set-up
    /// (the init and main part, run first) had already written.
    pub writes: BTreeMap<(u8, u8), u8>,
    /// How the conversation ended.
    pub end: End,
}

/// Run the whole conversation with `entity` (every window handed on without an answer, so a question takes its
/// cursor's starting line) and report what it wrote: the character's own set-up decides first whether it can be
/// talked to in the story `env` tells (as [`TalkSession::talk`] does), then its talk script runs to its end.
pub fn run_talk(script: &FieldScript<'_>, entity: usize, env: &dyn StoryVars, seed: u32) -> Result<TalkRun, TalkError> {
    if !env.execution_supported() { return Err(TalkError::UnsupportedStory); }
    if entity >= script.entity_count() {
        return Err(TalkError::NoSuchEntity);
    }
    let (verdict, setup) = prelude(script, env, entity, Limits::default());
    if !(verdict.talkable && verdict.visible) {
        return Err(TalkError::NotTalkable);
    }
    let mut walk = Walk::talk(entity, seed, Limits::default()).with_writes(setup.clone());
    let end = loop {
        if let Beat::End(end) = walk.resume(script, env, None) {
            break end;
        }
    };
    let writes = walk.writes.into_iter().filter(|(key, value)| setup.get(key) != Some(value)).collect();
    Ok(TalkRun { writes, end })
}

/// Why a conversation could not be started.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum TalkError {
    #[error("this story format is not supported by the dialogue interpreter")]
    UnsupportedStory,
    #[error("the field's script section cannot be read")]
    BadSection,
    #[error("the field has no such entity")]
    NoSuchEntity,
    #[error("the character is hidden or cannot be talked to at this point of the story")]
    NotTalkable,
}

/// A walk that owns the field's script section and its story hook: what a window holds on to.
pub struct TalkSession {
    section: Vec<u8>,
    walk: Walk,
    vars: Box<dyn StoryVars>,
}

impl TalkSession {
    /// The conversation with `entity`: its talk script, unless its own set-up has hidden it or
    /// switched talking off for the story as `vars` tells it.
    pub fn talk(section: Vec<u8>, entity: usize, vars: Box<dyn StoryVars>, seed: u32) -> Result<Self, TalkError> {
        if !vars.execution_supported() { return Err(TalkError::UnsupportedStory); }
        let (verdict, writes) = {
            let script = FieldScript::parse(&section).map_err(|_| TalkError::BadSection)?;
            if entity >= script.entity_count() {
                return Err(TalkError::NoSuchEntity);
            }
            prelude(&script, &*vars, entity, Limits::default())
        };
        if !(verdict.talkable && verdict.visible) {
            return Err(TalkError::NotTalkable);
        }
        let walk = Walk::talk(entity, seed, Limits::default()).with_writes(writes);
        Ok(Self { section, walk, vars })
    }

    /// Script `slot` of `entity`, whatever it is (the lines a character speaks in a cutscene), with
    /// no check of whether it can be talked to.
    pub fn script(section: Vec<u8>, entity: usize, slot: usize, vars: Box<dyn StoryVars>, seed: u32) -> Result<Self, TalkError> {
        if !vars.execution_supported() { return Err(TalkError::UnsupportedStory); }
        let script = FieldScript::parse(&section).map_err(|_| TalkError::BadSection)?;
        if entity >= script.entity_count() {
            return Err(TalkError::NoSuchEntity);
        }
        drop(script);
        Ok(Self { section, walk: Walk::script(entity, slot, seed, Limits::default()), vars })
    }

    /// The walk, for its counters.
    pub fn walk(&self) -> &Walk {
        &self.walk
    }
}

impl std::fmt::Debug for TalkSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TalkSession").field("section_bytes", &self.section.len()).field("walk", &self.walk).finish_non_exhaustive()
    }
}

impl MessageSource for TalkSession {
    fn next(&mut self, answer: Option<usize>) -> Beat {
        match FieldScript::parse(&self.section) {
            Ok(script) => self.walk.resume(&script, &*self.vars, answer),
            Err(_) => Beat::End(End::Broken),
        }
    }
}
