//! Instruction decoder and disassembler for field scripts.
//!
//! The byte layout of every opcode lives in [`super::opcodes`]; this module walks a script
//! instruction by instruction, turns the operand bytes into [`Operand`]s (resolving the bank
//! nibbles that say whether an operand is a literal or a variable address) and resolves jump
//! targets.
//!
//! **Operands.** Most opcodes that take numbers prefix them with bank bytes: each nibble (high
//! nibble first) belongs to the next banked operand; nibble `0` means the operand is the literal
//! stored in the instruction, any other value means the stored number is the address of a
//! variable in that bank (see [`Operand::bank`]). The disassembler does not evaluate variables.
//!
//! **Jumps.** `JMPF`/`JMPFL` and the `IF*` family jump forward by an unsigned distance
//! measured from the first byte of the jump operand itself; `JMPB`/`JMPBL` jump backward by a
//! distance measured from the opcode byte (FFRTT "FF7/Field/Script/Opcodes" pages for `IFUB`,
//! `JMPF` and `JMPB`, confirmed on the owned data: with these bases every jump target of every
//! script lands on an instruction boundary). The resolved, section-relative target is in
//! [`Instruction::jump_target`].

use super::opcodes::{self, Fld};
use crate::util::{slice_at, u16_at, u8_at};
use std::fmt;
use thiserror::Error;

/// One operand of a decoded instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Operand {
    /// Name from the opcode documentation (`"x"`, `"char_id"`, `"jump"`, ...).
    pub name: &'static str,
    /// The stored number, sign-extended for signed types. When `bank != 0` this is a variable
    /// address (the low byte of the stored number).
    pub value: i32,
    /// Governing bank nibble: `0` = `value` is the operand itself, otherwise the operand is the
    /// variable at address `value` of bank `bank`.
    pub bank: u8,
}

impl Operand {
    /// The operand's value if it is a literal (bank 0), `None` if it names a variable.
    pub fn literal(&self) -> Option<i32> {
        (self.bank == 0).then_some(self.value)
    }
}

/// A decoded instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instruction {
    /// Section-relative offset of the opcode byte.
    pub offset: usize,
    /// Total length in bytes, opcode byte included.
    pub len: usize,
    pub opcode: u8,
    /// Mnemonic of the opcode (`"SPECIAL"` / `"KAWAI"` for the multi-byte families).
    pub mnemonic: &'static str,
    /// Sub-opcode and its mnemonic for `SPECIAL` (`0F`) and `KAWAI` (`28`); `None` otherwise.
    pub sub: Option<(u8, &'static str)>,
    pub operands: Vec<Operand>,
    /// Section-relative jump target of `JMP*`/`IF*` instructions (`None` for other opcodes, and
    /// for a jump whose target would be negative).
    pub jump_target: Option<usize>,
    /// Argument bytes of the variable-length `KAWAI` opcode (empty for every other opcode).
    pub payload: Vec<u8>,
    /// Diagnostic: a bank nibble that no operand of this opcode uses is non-zero. Always
    /// `false` in the owned data when the opcode's bank layout is right.
    pub stray_banks: bool,
}

impl Instruction {
    /// Operand called `name`.
    pub fn operand(&self, name: &str) -> Option<&Operand> {
        self.operands.iter().find(|o| o.name == name)
    }

    /// Literal value of operand `name` (`None` if absent or banked).
    pub fn literal(&self, name: &str) -> Option<i32> {
        self.operand(name).and_then(Operand::literal)
    }

    /// Offset of the instruction that follows this one.
    pub fn end(&self) -> usize {
        self.offset.saturating_add(self.len)
    }

    /// Whether the instruction can transfer control to a jump target.
    pub fn is_jump(&self) -> bool {
        opcodes::is_jump(self.opcode)
    }

    /// Whether control can never fall through to the next instruction.
    pub fn ends_flow(&self) -> bool {
        opcodes::ends_flow(self.opcode)
    }
}

impl fmt::Display for Instruction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04X}: {}", self.offset, self.mnemonic)?;
        if let Some((code, name)) = self.sub {
            write!(f, ".{name}({code:02X})")?;
        }
        for o in &self.operands {
            if o.name == "sub" {
                continue;
            }
            if o.bank == 0 {
                write!(f, " {}={}", o.name, o.value)?;
            } else {
                write!(f, " {}=Var[{}][{}]", o.name, o.bank, o.value & 0xFF)?;
            }
        }
        if let Some(t) = self.jump_target {
            write!(f, " -> {t:04X}")?;
        }
        if !self.payload.is_empty() {
            write!(f, " +{} payload bytes", self.payload.len())?;
        }
        Ok(())
    }
}

/// Why a script could not be decoded further.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// The byte is not an assigned opcode.
    #[error("unknown opcode 0x{opcode:02X} at offset {offset:#06X}")]
    UnknownOpcode { offset: usize, opcode: u8 },
    /// A `SPECIAL` sub-opcode that has no documented layout (its length is unknown).
    #[error("unknown SPECIAL sub-opcode 0x{sub:02X} at offset {offset:#06X}")]
    UnknownSubOpcode { offset: usize, sub: u8 },
    /// The instruction's operands run past the end of the script.
    #[error("opcode 0x{opcode:02X} at offset {offset:#06X} needs {needed} bytes but only {available} remain in the script")]
    Truncated { offset: usize, opcode: u8, needed: usize, available: usize },
    /// A `KAWAI` instruction whose length byte is smaller than its own header.
    #[error("KAWAI at offset {offset:#06X} declares an impossible length of {declared}")]
    BadLength { offset: usize, declared: usize },
}

impl DecodeError {
    /// Offset of the instruction that failed.
    pub fn offset(&self) -> usize {
        match self {
            Self::UnknownOpcode { offset, .. }
            | Self::UnknownSubOpcode { offset, .. }
            | Self::Truncated { offset, .. }
            | Self::BadLength { offset, .. } => *offset,
        }
    }
}

/// How a fully decoded script ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ScriptEnd {
    /// No instructions at all.
    Empty,
    /// `RET`.
    Ret,
    /// `RETTO`.
    Retto,
    /// An unconditional backward jump (`JMPB`/`JMPBL`): a loop with no return.
    Loop,
    /// An unconditional forward jump.
    ForwardJump,
    /// `GAMEOVER`.
    GameOver,
    /// Anything else (the script just stops, e.g. after padding).
    Other(u8),
}

impl ScriptEnd {
    /// Classify the last instruction of `d`.
    pub fn of(d: &Disassembly) -> Self {
        match d.instructions.last().map(|i| i.opcode) {
            None => Self::Empty,
            Some(0x00) => Self::Ret,
            Some(0x07) => Self::Retto,
            Some(0x12 | 0x13) => Self::Loop,
            Some(0x10 | 0x11) => Self::ForwardJump,
            Some(0xFF) => Self::GameOver,
            Some(other) => Self::Other(other),
        }
    }
}

/// A script walked as far as it could be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Disassembly {
    pub instructions: Vec<Instruction>,
    /// Why decoding stopped early; `None` when the whole slice decoded.
    pub error: Option<DecodeError>,
}

impl Disassembly {
    /// Whether every byte of the slice belongs to a decoded instruction.
    pub fn is_complete(&self) -> bool {
        self.error.is_none()
    }

    /// Offset one past the last decoded instruction (`base` for an empty listing).
    pub fn end_offset(&self, base: usize) -> usize {
        self.instructions.last().map_or(base, Instruction::end)
    }

    /// Jump instructions whose target is not the start of a decoded instruction, as
    /// `(jump instruction offset, target)`. `usize::MAX` stands for a target before the section
    /// start. A target equal to `end` is accepted only when `allow_end` is set.
    pub fn bad_jumps(&self, end: usize, allow_end: bool) -> Vec<(usize, usize)> {
        let starts: std::collections::BTreeSet<usize> = self.instructions.iter().map(|i| i.offset).collect();
        self.instructions
            .iter()
            .filter(|i| i.is_jump())
            .filter_map(|i| {
                let ok = i.jump_target.is_some_and(|t| starts.contains(&t) || (allow_end && t == end));
                (!ok).then(|| (i.offset, i.jump_target.unwrap_or(usize::MAX)))
            })
            .collect()
    }
}

/// Direction of a relative jump operand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JumpDir {
    /// Distance added to the address of the jump operand.
    Forward,
    /// Distance subtracted from the address of the opcode.
    Backward,
}

struct JumpInfo {
    dir: JumpDir,
    distance: u32,
    /// Position of the jump operand's first byte within the script slice.
    operand_pos: usize,
}

struct Decoder<'a> {
    code: &'a [u8],
    base: usize,
    start: usize,
    opcode: u8,
    cur: usize,
    operands: Vec<Operand>,
    /// Bank nibbles read so far (high nibble of the first bank byte first).
    nibbles: [u8; 16],
    nibble_count: usize,
    /// Bit `i` set: nibble `i` was assigned to an operand.
    used_nibbles: u32,
    jump: Option<JumpInfo>,
}

impl<'a> Decoder<'a> {
    fn new(code: &'a [u8], base: usize, start: usize, opcode: u8) -> Self {
        Self {
            code,
            base,
            start,
            opcode,
            cur: start + 1,
            operands: Vec::new(),
            nibbles: [0; 16],
            nibble_count: 0,
            used_nibbles: 0,
            jump: None,
        }
    }

    fn offset(&self) -> usize {
        self.base.saturating_add(self.start)
    }

    fn truncated(&self, want: usize) -> DecodeError {
        DecodeError::Truncated {
            offset: self.offset(),
            opcode: self.opcode,
            needed: self.cur.saturating_sub(self.start).saturating_add(want),
            available: self.code.len().saturating_sub(self.start),
        }
    }

    /// Fail with the instruction's full length when fewer than `total` bytes remain.
    fn require(&self, total: usize) -> Result<(), DecodeError> {
        let available = self.code.len().saturating_sub(self.start);
        if available < total {
            return Err(DecodeError::Truncated { offset: self.offset(), opcode: self.opcode, needed: total, available });
        }
        Ok(())
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        let s = slice_at(self.code, self.cur, n).ok_or_else(|| self.truncated(n))?;
        self.cur += n; // slice_at succeeded, so this cannot overflow
        Ok(s)
    }

    fn u8(&mut self) -> Result<u8, DecodeError> {
        let s = self.take(1)?;
        Ok(s.first().copied().unwrap_or(0))
    }

    fn u16(&mut self) -> Result<u16, DecodeError> {
        let s = self.take(2)?;
        Ok(u16_at(s, 0).unwrap_or(0))
    }

    /// Bank nibble `index` of the bank bytes read so far; marks it as assigned to an operand.
    fn nibble(&mut self, index: u8) -> u8 {
        let index = usize::from(index);
        if index < 32 {
            self.used_nibbles |= 1 << index;
        }
        self.nibbles.get(index).copied().unwrap_or(0)
    }

    fn push(&mut self, name: &'static str, value: i32, bank: u8) {
        self.operands.push(Operand { name, value, bank });
    }

    fn run(&mut self, fields: &[Fld]) -> Result<(), DecodeError> {
        for fld in fields {
            match *fld {
                Fld::Banks(bytes) => {
                    let raw = self.take(usize::from(bytes))?;
                    for (k, &b) in raw.iter().enumerate() {
                        if let Some(slot) = self.nibbles.get_mut(k * 2) {
                            *slot = b >> 4;
                        }
                        if let Some(slot) = self.nibbles.get_mut(k * 2 + 1) {
                            *slot = b & 0x0F;
                        }
                    }
                    self.nibble_count = (self.nibble_count + usize::from(bytes) * 2).min(self.nibbles.len());
                }
                Fld::Bu8(name, idx) => {
                    let bank = self.nibble(idx);
                    let v = self.u8()?;
                    self.push(name, i32::from(v), bank);
                }
                Fld::Bu16(name, idx) => {
                    let bank = self.nibble(idx);
                    let v = self.u16()?;
                    self.push(name, i32::from(v), bank);
                }
                Fld::Bs16(name, idx) => {
                    let bank = self.nibble(idx);
                    let v = self.u16()?;
                    self.push(name, i32::from(i16::from_le_bytes(v.to_le_bytes())), bank);
                }
                Fld::Bs32(name, idx) => {
                    let bank = self.nibble(idx);
                    let raw = self.take(4)?;
                    let b: [u8; 4] = raw.try_into().unwrap_or([0; 4]);
                    self.push(name, i32::from_le_bytes(b), bank);
                }
                Fld::PrioScript => {
                    let v = self.u8()?;
                    self.push("priority", i32::from(v >> 5), 0);
                    self.push("script", i32::from(v & 0x1F), 0);
                }
                Fld::U8(name) => {
                    let v = self.u8()?;
                    self.push(name, i32::from(v), 0);
                }
                Fld::U16(name) => {
                    let v = self.u16()?;
                    self.push(name, i32::from(v), 0);
                }
                Fld::S16(name) => {
                    let v = self.u16()?;
                    self.push(name, i32::from(i16::from_le_bytes(v.to_le_bytes())), 0);
                }
                Fld::S32(name) => {
                    let raw = self.take(4)?;
                    let b: [u8; 4] = raw.try_into().unwrap_or([0; 4]);
                    self.push(name, i32::from_le_bytes(b), 0);
                }
                Fld::Jump(name, dir, width) => {
                    let operand_pos = self.cur;
                    let distance = if width == 1 { u32::from(self.u8()?) } else { u32::from(self.u16()?) };
                    self.push(name, i32::try_from(distance).unwrap_or(i32::MAX), 0);
                    self.jump = Some(JumpInfo { dir, distance, operand_pos });
                }
            }
        }
        Ok(())
    }

    fn finish(self, mnemonic: &'static str, sub: Option<(u8, &'static str)>, payload: Vec<u8>) -> Instruction {
        let stray_banks = (0..self.nibble_count).any(|i| self.used_nibbles & (1u32 << i) == 0 && self.nibbles.get(i).copied().unwrap_or(0) != 0);
        let jump_target = self.jump.as_ref().and_then(|j| {
            let anchor = match j.dir {
                JumpDir::Forward => j.operand_pos,
                JumpDir::Backward => self.start,
            };
            let anchor = self.base.checked_add(anchor)?;
            let distance = usize::try_from(j.distance).ok()?;
            match j.dir {
                JumpDir::Forward => anchor.checked_add(distance),
                JumpDir::Backward => anchor.checked_sub(distance),
            }
        });
        Instruction {
            offset: self.offset(),
            len: self.cur - self.start,
            opcode: self.opcode,
            mnemonic,
            sub,
            operands: self.operands,
            jump_target,
            payload,
            stray_banks,
        }
    }
}

/// Decode the instruction at `code[pos..]`. `base` is the section offset of `code[0]`, so the
/// returned offsets and jump targets are section-relative.
pub fn decode_at(code: &[u8], pos: usize, base: usize) -> Result<Instruction, DecodeError> {
    let offset = base.saturating_add(pos);
    let opcode = u8_at(code, pos).ok_or(DecodeError::Truncated { offset, opcode: 0, needed: 1, available: 0 })?;
    let mut d = Decoder::new(code, base, pos, opcode);
    match opcode {
        opcodes::KAWAI => {
            // [28][L][S][args...]: L is the length of the whole instruction.
            let declared = usize::from(d.u8()?);
            if declared < 3 {
                return Err(DecodeError::BadLength { offset, declared });
            }
            let available = code.len().saturating_sub(pos);
            if declared > available {
                return Err(DecodeError::Truncated { offset, opcode, needed: declared, available });
            }
            let sub = d.u8()?;
            d.push("sub", i32::from(sub), 0);
            let payload_len = declared - 3;
            let payload = slice_at(code, d.cur, payload_len).ok_or(DecodeError::Truncated { offset, opcode, needed: declared, available })?.to_vec();
            // sub-opcodes with a documented layout get named operands read from the payload
            let layout = opcodes::kawai_sub_layout(sub);
            if opcodes::layout_width(layout) <= payload_len {
                d.run(layout)?;
            }
            d.cur = pos + declared;
            Ok(d.finish("KAWAI", Some((sub, opcodes::kawai_sub_name(sub))), payload))
        }
        opcodes::SPECIAL => {
            let sub = d.u8()?;
            let (name, fields) = opcodes::special_info(sub).ok_or(DecodeError::UnknownSubOpcode { offset, sub })?;
            d.require(2 + opcodes::layout_width(fields))?;
            d.push("sub", i32::from(sub), 0);
            d.run(fields)?;
            Ok(d.finish("SPECIAL", Some((sub, name)), Vec::new()))
        }
        _ => {
            let info = opcodes::info(opcode).ok_or(DecodeError::UnknownOpcode { offset, opcode })?;
            d.require(1 + opcodes::layout_width(info.fields))?;
            d.run(info.fields)?;
            Ok(d.finish(info.mnemonic, None, Vec::new()))
        }
    }
}

/// Walk `code` (whose first byte is at section offset `base`) from the start, stopping at the
/// first byte that cannot be decoded.
pub fn disassemble_slice(code: &[u8], base: usize) -> Disassembly {
    let mut instructions = Vec::new();
    let mut pos = 0usize;
    while pos < code.len() {
        match decode_at(code, pos, base) {
            Ok(instr) => {
                pos += instr.len.max(1);
                instructions.push(instr);
            }
            Err(e) => return Disassembly { instructions, error: Some(e) },
        }
    }
    Disassembly { instructions, error: None }
}
