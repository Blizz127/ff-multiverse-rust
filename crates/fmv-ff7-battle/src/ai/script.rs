//! Where a scene keeps its AI scripts, and the decoder for one instruction.
//!
//! # Layout of the AI section (scene offset `0x0C80` to the end of the scene)
//!
//! | offset | size | content |
//! |---|---|---|
//! | `0x0C80` | 4 x u16 | formation AI offsets (not used here) |
//! | `0x0C88` | up to 504 | formation AI data (not used here) |
//! | `0x0E80` | 3 x u16 | the offset of each enemy's AI block, **relative to `0x0E80`** (`0xFFFF`: none) |
//! | `0x0E86` | the rest | the enemies' AI blocks, one after the other |
//!
//! An enemy's block starts with sixteen u16 pointers, relative to the start of the block (`0xFFFF`: no
//! script in that slot; the slots are listed in [`slot`]), and the scripts follow. A script is bytecode that
//! ends at an `END` instruction; **jump targets are relative to the first byte of the script they are in**.
//!
//! Sources: <https://wiki.ffrtt.ru/index.php/FF7/Battle/Battle_Scenes> (the layout),
//! <https://wiki.ffrtt.ru/index.php/FF7/Battle/Battle_Scenes/Battle_Script> (the opcodes, from Terence
//! Fergusson's notes). Checked against the owner's own `scene.bin` (read only) by this crate's
//! `#[ignore]`d real-data tests: the offsets above hold for all 256 scenes, and following every jump
//! from the start of every script of every enemy decodes cleanly (the jump base is the script, not the
//! block: with the block as the base most scripts decode into garbage).
//!
//! # Instruction encoding
//!
//! One opcode byte, then its inline arguments:
//!
//! | opcode | arguments | meaning |
//! |---|---|---|
//! | `0x00..=0x03` | u16 address | push the value at the address, of size X: 0 bit, 1 byte, 2 word, 3 dword |
//! | `0x10..=0x13` | u16 address | push the address itself (a place to store to), of size X |
//! | `0x30..=0x37` | none | add, subtract, multiply, divide, remainder, AND, OR, NOT |
//! | `0x40..=0x45` | none | `==`, `!=`, `>=`, `<=`, `>`, `<` |
//! | `0x50..=0x52` | none | logical AND, OR, NOT |
//! | `0x60..=0x62` | 1, 2, 3 bytes | push a constant |
//! | `0x70`, `0x71`, `0x72` | u16 target | jump if zero, jump if not equal to the top of the stack, jump |
//! | `0x73` | none | end of the script |
//! | `0x80..=0x87` | none | mask, random word, random bit, bit count, greatest, least, MP cost, one bit |
//! | `0x90..=0x92` | none | store, drop, act (perform an attack) |
//! | `0x93` | FF text up to `0xFF` | show a text |
//! | `0x74`, `0x75`, `0x94..=0x96`, `0xA1` | none | documented, never needed by the supported scripts |
//! | `0xA0` | 1 byte, ASCII up to `0x00` | print to the debug console (appears only in test scenes) |
//!
//! Every other opcode byte is refused here ([`DecodeError::Unknown`]); the original engine skips some
//! groups as no-ops, but a script that contains one is not understood, so the caller falls back.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::bytes::u16_at;

/// Scene offset of the table of the enemies' AI block offsets.
pub const ENEMY_TABLE: usize = 0x0E80;
/// Enemies a scene has AI blocks for.
pub const ENEMIES: usize = 3;
/// Bytes of the table (the smallest valid block offset).
pub const ENEMY_TABLE_BYTES: usize = ENEMIES * 2;
/// Script slots in a block.
pub const SLOTS: usize = 16;
/// Bytes of a block's pointer header.
pub const HEADER_BYTES: usize = SLOTS * 2;
/// "No script" in a pointer.
pub const NONE: u16 = 0xFFFF;

/// What the sixteen script slots of an enemy are for.
pub mod slot {
    /// Runs once before the battle.
    pub const INITIALIZE: usize = 0;
    /// Runs when the enemy's gauge is full: it decides what the enemy does.
    pub const MAIN: usize = 1;
    pub const GENERAL_COUNTER: usize = 2;
    pub const DEATH_COUNTER: usize = 3;
    pub const PHYSICAL_COUNTER: usize = 4;
    pub const MAGICAL_COUNTER: usize = 5;
    pub const BATTLE_END: usize = 6;
    pub const PRE_ACTION: usize = 7;
    /// Slots 8..=15 are custom events, run only by another script's "run script" command.
    pub const CUSTOM_FIRST: usize = 8;
}

/// Opcode bytes.
pub mod op {
    pub const ADD: u8 = 0x30;
    pub const SUB: u8 = 0x31;
    pub const MUL: u8 = 0x32;
    pub const DIV: u8 = 0x33;
    pub const MOD: u8 = 0x34;
    pub const AND: u8 = 0x35;
    pub const OR: u8 = 0x36;
    pub const NOT: u8 = 0x37;
    pub const EQ: u8 = 0x40;
    pub const NE: u8 = 0x41;
    pub const GE: u8 = 0x42;
    pub const LE: u8 = 0x43;
    pub const GT: u8 = 0x44;
    pub const LT: u8 = 0x45;
    pub const LAND: u8 = 0x50;
    pub const LOR: u8 = 0x51;
    pub const LNOT: u8 = 0x52;
    pub const JUMP_IF_ZERO: u8 = 0x70;
    pub const JUMP_IF_NE: u8 = 0x71;
    pub const JUMP: u8 = 0x72;
    pub const END: u8 = 0x73;
    pub const DROP_74: u8 = 0x74;
    pub const LINK: u8 = 0x75;
    pub const MASK: u8 = 0x80;
    pub const RANDOM: u8 = 0x81;
    pub const RANDOM_BIT: u8 = 0x82;
    pub const BIT_COUNT: u8 = 0x83;
    pub const GREATEST: u8 = 0x84;
    pub const LEAST: u8 = 0x85;
    pub const MP_COST: u8 = 0x86;
    pub const BIT: u8 = 0x87;
    pub const STORE: u8 = 0x90;
    pub const DROP: u8 = 0x91;
    pub const ACT: u8 = 0x92;
    pub const TEXT: u8 = 0x93;
    pub const COPY: u8 = 0x94;
    pub const MEMORY: u8 = 0x95;
    pub const ELEMENT_DEFENSE: u8 = 0x96;
    pub const DEBUG: u8 = 0xA0;
    pub const DROP_TWO: u8 = 0xA1;
}

/// One decoded instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Insn {
    /// Push the value at `addr` (`size`: 0 bit, 1 byte, 2 word, 3 dword).
    PushValue { size: u8, addr: u16 },
    /// Push the address `addr` itself, to be stored to.
    PushAddr { size: u8, addr: u16 },
    /// Push a constant (`size` 1, 2 or 3 bytes).
    Const { size: u8, value: u32 },
    /// `JUMP_IF_ZERO`, `JUMP_IF_NE` or `JUMP`, to `target` (relative to the script's first byte).
    Jump { op: u8, target: u16 },
    /// Show a text; `len` bytes of text including its terminator.
    Text { len: u16 },
    /// Debug-console print (the whole instruction is `len` bytes after the opcode).
    Debug { len: u16 },
    /// Any instruction with no inline arguments (the opcode byte).
    Simple(u8),
}

/// Why an instruction could not be decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    #[error("the script runs off the end of its block")]
    Truncated,
    #[error("unknown opcode {0:#04x}")]
    Unknown(u8),
    #[error("jump target {0:#05x} is outside the block")]
    BadJump(u16),
}

fn at(code: &[u8], pc: usize) -> Result<u8, DecodeError> {
    code.get(pc).copied().ok_or(DecodeError::Truncated)
}

fn word_at(code: &[u8], pc: usize) -> Result<u16, DecodeError> {
    at(code, pc + 1)?;
    at(code, pc + 2)?;
    Ok(u16_at(code, pc + 1))
}

/// Decode the instruction at `pc`: the instruction and the number of bytes it takes. Total: never panics.
pub fn decode(code: &[u8], pc: usize) -> Result<(Insn, usize), DecodeError> {
    let opcode = at(code, pc)?;
    Ok(match opcode {
        0x00..=0x03 => (
            Insn::PushValue {
                size: opcode & 3,
                addr: word_at(code, pc)?,
            },
            3,
        ),
        0x10..=0x13 => (
            Insn::PushAddr {
                size: opcode & 3,
                addr: word_at(code, pc)?,
            },
            3,
        ),
        0x30..=0x37
        | 0x40..=0x45
        | 0x50..=0x52
        | 0x73..=0x75
        | 0x80..=0x87
        | 0x90..=0x92
        | 0x94..=0x96
        | 0xA1 => (Insn::Simple(opcode), 1),
        0x60 => (
            Insn::Const {
                size: 1,
                value: u32::from(at(code, pc + 1)?),
            },
            2,
        ),
        0x61 => (
            Insn::Const {
                size: 2,
                value: u32::from(word_at(code, pc)?),
            },
            3,
        ),
        0x62 => {
            at(code, pc + 3)?;
            let value = u32::from(at(code, pc + 1)?)
                | u32::from(at(code, pc + 2)?) << 8
                | u32::from(at(code, pc + 3)?) << 16;
            (Insn::Const { size: 3, value }, 4)
        }
        0x70..=0x72 => (
            Insn::Jump {
                op: opcode,
                target: word_at(code, pc)?,
            },
            3,
        ),
        0x93 => {
            // FF text up to the first 0xFF (which is part of the instruction).
            let rest = code.get(pc + 1..).ok_or(DecodeError::Truncated)?;
            let end = rest
                .iter()
                .position(|&b| b == 0xFF)
                .ok_or(DecodeError::Truncated)?;
            let len = end + 1;
            (Insn::Text { len: len as u16 }, 1 + len)
        }
        0xA0 => {
            // One argument byte, then ASCII up to a 0x00.
            let rest = code.get(pc + 2..).ok_or(DecodeError::Truncated)?;
            let end = rest
                .iter()
                .position(|&b| b == 0)
                .ok_or(DecodeError::Truncated)?;
            let len = 1 + end + 1;
            (Insn::Debug { len: len as u16 }, 1 + len)
        }
        other => return Err(DecodeError::Unknown(other)),
    })
}

/// The scripts of one enemy: its block of the scene's AI section.
#[derive(Clone, PartialEq, Eq)]
pub struct EnemyScripts {
    code: Vec<u8>,
    entries: [Option<u16>; SLOTS],
}

impl std::fmt::Debug for EnemyScripts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnemyScripts")
            .field("bytes", &self.code.len())
            .field("slots", &self.slots().collect::<Vec<_>>())
            .finish()
    }
}

impl EnemyScripts {
    /// Parse an enemy's block (its pointer header and everything up to the next block or the end of the
    /// scene). Pointers that do not point into the block's code are dropped. Total: never panics.
    pub fn parse_block(block: &[u8]) -> Self {
        let mut entries = [None; SLOTS];
        for (i, entry) in entries.iter_mut().enumerate() {
            let p = u16_at(block, i * 2);
            if p != NONE && usize::from(p) >= HEADER_BYTES && usize::from(p) < block.len() {
                *entry = Some(p);
            }
        }
        Self {
            code: block.to_vec(),
            entries,
        }
    }

    /// The bytes of the block (the pointer header included; offsets in [`Self::entry`] index them).
    pub fn code(&self) -> &[u8] {
        &self.code
    }

    /// Offset in [`Self::code`] of the first byte of the script in `slot`, if it has one.
    pub fn entry(&self, slot: usize) -> Option<usize> {
        self.entries.get(slot).copied().flatten().map(usize::from)
    }

    /// The slots that have a script.
    pub fn slots(&self) -> impl Iterator<Item = usize> + '_ {
        (0..SLOTS).filter(|&s| self.entry(s).is_some())
    }

    /// Every instruction reachable from the start of the script in `slot`, by offset, following every
    /// jump (both ways of a conditional one). Fails on the first thing that cannot be decoded.
    pub fn reachable(&self, slot: usize) -> Result<BTreeMap<usize, Insn>, DecodeError> {
        let entry = self.entry(slot).ok_or(DecodeError::Truncated)?;
        let mut seen: BTreeMap<usize, Insn> = BTreeMap::new();
        let mut work = vec![entry];
        while let Some(mut pc) = work.pop() {
            loop {
                if seen.contains_key(&pc) {
                    break;
                }
                let (insn, len) = decode(&self.code, pc)?;
                seen.insert(pc, insn);
                match insn {
                    Insn::Jump { op, target } => {
                        let to = entry + usize::from(target);
                        if to >= self.code.len() {
                            return Err(DecodeError::BadJump(target));
                        }
                        work.push(to);
                        if op == op::JUMP {
                            break;
                        }
                    }
                    Insn::Simple(op::END) => break,
                    _ => {}
                }
                pc += len;
            }
        }
        Ok(seen)
    }
}

/// The AI scripts of a scene: one block per enemy record.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SceneAi {
    enemies: [Option<Arc<EnemyScripts>>; ENEMIES],
}

impl SceneAi {
    /// Find the enemies' blocks in a decompressed scene. Blocks whose offset is out of range are ignored.
    pub fn parse(scene: &[u8]) -> Self {
        let mut starts: Vec<(usize, usize)> = Vec::new();
        for slot in 0..ENEMIES {
            let off = u16_at(scene, ENEMY_TABLE + slot * 2);
            if off == NONE || usize::from(off) < ENEMY_TABLE_BYTES {
                continue;
            }
            let start = ENEMY_TABLE + usize::from(off);
            if start + HEADER_BYTES <= scene.len() {
                starts.push((slot, start));
            }
        }
        let mut ends: Vec<usize> = starts.iter().map(|&(_, s)| s).collect();
        ends.sort_unstable();
        let mut enemies: [Option<Arc<EnemyScripts>>; ENEMIES] = Default::default();
        for &(slot, start) in &starts {
            let end = ends
                .iter()
                .copied()
                .find(|&e| e > start)
                .unwrap_or(scene.len());
            if let (Some(block), Some(cell)) = (scene.get(start..end), enemies.get_mut(slot)) {
                *cell = Some(Arc::new(EnemyScripts::parse_block(block)));
            }
        }
        Self { enemies }
    }

    /// The scripts of the enemy record in `slot` (0..3).
    pub fn enemy(&self, slot: usize) -> Option<&Arc<EnemyScripts>> {
        self.enemies.get(slot).and_then(Option::as_ref)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_each_instruction_shape() {
        let code = [
            0x12, 0x70, 0x20, // push address 0x2070, word
            0x01, 0x40, 0x00, // push byte at 0x0040
            0x60, 0x07, // constant 7
            0x61, 0x34, 0x12, // constant 0x1234
            0x62, 0x56, 0x34, 0x12, // constant 0x123456
            0x72, 0x05, 0x00, // jump
            0x93, 0x21, 0x22, 0xFF, // text "AB"
            0xA0, 0x01, b'x', 0x00, // debug
            0x92, // act
        ];
        let mut pc = 0;
        let mut got = Vec::new();
        while pc < code.len() {
            let (insn, len) = decode(&code, pc).unwrap();
            got.push(insn);
            pc += len;
        }
        assert_eq!(pc, code.len());
        assert_eq!(
            got,
            vec![
                Insn::PushAddr { size: 2, addr: 0x2070 },
                Insn::PushValue { size: 1, addr: 0x0040 },
                Insn::Const { size: 1, value: 7 },
                Insn::Const { size: 2, value: 0x1234 },
                Insn::Const { size: 3, value: 0x12_3456 },
                Insn::Jump { op: op::JUMP, target: 5 },
                Insn::Text { len: 3 },
                Insn::Debug { len: 3 },
                Insn::Simple(op::ACT),
            ]
        );
    }

    #[test]
    fn decoding_is_total() {
        // Truncated arguments, unterminated texts, unknown opcodes, and every opcode byte at the very end.
        assert_eq!(decode(&[], 0), Err(DecodeError::Truncated));
        assert_eq!(decode(&[0x02, 0x10], 0), Err(DecodeError::Truncated));
        assert_eq!(decode(&[0x62, 1, 2], 0), Err(DecodeError::Truncated));
        assert_eq!(decode(&[0x93, 0x21, 0x22], 0), Err(DecodeError::Truncated));
        assert_eq!(decode(&[0xA0, 0x01, b'x'], 0), Err(DecodeError::Truncated));
        assert_eq!(decode(&[0xFF], 0), Err(DecodeError::Unknown(0xFF)));
        assert_eq!(decode(&[0x20], 0), Err(DecodeError::Unknown(0x20)));
        assert_eq!(decode(&[0x73], usize::MAX), Err(DecodeError::Truncated));
        for opcode in 0..=255u8 {
            for tail in [&[][..], &[0u8][..], &[0xFF, 0xFF, 0xFF, 0xFF][..]] {
                let mut code = vec![opcode];
                code.extend_from_slice(tail);
                if let Ok((_, len)) = decode(&code, 0) {
                    assert!(len >= 1 && len <= code.len(), "{opcode:#04x} {tail:?}");
                }
            }
        }
    }

    /// A block: the pointer header with `main` in slot 1 followed by `code` at offset 32.
    fn block_with_main(code: &[u8]) -> Vec<u8> {
        let mut block = vec![0xFF; HEADER_BYTES];
        block[2..4].copy_from_slice(&(HEADER_BYTES as u16).to_le_bytes());
        block.extend_from_slice(code);
        block
    }

    #[test]
    fn a_block_finds_its_scripts_and_follows_jumps_from_the_script_start() {
        // 0: push 1; 2: jump-if-zero -> 9 (relative to the script start); 5: act; 6: jump -> 9; 9: end
        let code = [0x60, 0x01, 0x70, 0x09, 0x00, 0x92, 0x72, 0x09, 0x00, 0x73];
        let scripts = EnemyScripts::parse_block(&block_with_main(&code));
        assert_eq!(scripts.entry(slot::MAIN), Some(HEADER_BYTES));
        assert_eq!(scripts.entry(slot::INITIALIZE), None);
        assert_eq!(scripts.slots().collect::<Vec<_>>(), vec![slot::MAIN]);
        let insns = scripts.reachable(slot::MAIN).unwrap();
        let offsets: Vec<usize> = insns.keys().copied().collect();
        // 32 + {0, 2, 5, 6, 9}: the end at script offset 9 is reached by both jumps.
        assert_eq!(offsets, vec![32, 34, 37, 38, 41]);
        assert_eq!(insns[&41], Insn::Simple(op::END));
    }

    #[test]
    fn reachable_refuses_what_cannot_run() {
        // Jump out of the block.
        let scripts = EnemyScripts::parse_block(&block_with_main(&[0x72, 0xF0, 0x00]));
        assert_eq!(scripts.reachable(slot::MAIN), Err(DecodeError::BadJump(0xF0)));
        // Runs off the end without END.
        let scripts = EnemyScripts::parse_block(&block_with_main(&[0x60, 0x01]));
        assert_eq!(scripts.reachable(slot::MAIN), Err(DecodeError::Truncated));
        // Unknown opcode.
        let scripts = EnemyScripts::parse_block(&block_with_main(&[0xFF]));
        assert_eq!(scripts.reachable(slot::MAIN), Err(DecodeError::Unknown(0xFF)));
        // A loop is followed once, not forever.
        let scripts = EnemyScripts::parse_block(&block_with_main(&[0x72, 0x00, 0x00]));
        assert_eq!(scripts.reachable(slot::MAIN).unwrap().len(), 1);
    }

    #[test]
    fn bad_pointers_are_dropped() {
        let mut block = vec![0xFF; HEADER_BYTES];
        block[0..2].copy_from_slice(&4u16.to_le_bytes()); // inside the header
        block[2..4].copy_from_slice(&0x7000u16.to_le_bytes()); // far past the block
        block.push(0x73);
        let scripts = EnemyScripts::parse_block(&block);
        assert_eq!(scripts.slots().count(), 0);
        // Too short for a header: nothing, no panic.
        assert_eq!(EnemyScripts::parse_block(&[1, 2, 3]).slots().count(), 0);
        assert_eq!(EnemyScripts::parse_block(&[]).slots().count(), 0);
    }

    #[test]
    fn the_scene_table_gives_each_enemy_its_block() {
        let mut scene = vec![0xFF; 0x1E80];
        // Enemy 0's block at table + 6, enemy 2's at table + 0x40; enemy 1 has none.
        scene[ENEMY_TABLE..ENEMY_TABLE + 2].copy_from_slice(&6u16.to_le_bytes());
        scene[ENEMY_TABLE + 4..ENEMY_TABLE + 6].copy_from_slice(&0x40u16.to_le_bytes());
        let first = ENEMY_TABLE + 6;
        let second = ENEMY_TABLE + 0x40;
        scene[first + 2..first + 4].copy_from_slice(&0x20u16.to_le_bytes());
        scene[first + 0x20] = 0x73;
        scene[second + 2..second + 4].copy_from_slice(&0x20u16.to_le_bytes());
        scene[second + 0x20] = 0x73;
        let ai = SceneAi::parse(&scene);
        assert!(ai.enemy(1).is_none());
        let a = ai.enemy(0).unwrap();
        assert_eq!(a.code().len(), 0x40 - 6, "a block ends where the next one starts");
        assert_eq!(a.entry(slot::MAIN), Some(0x20));
        let b = ai.enemy(2).unwrap();
        assert_eq!(b.code().len(), 0x1E80 - second, "the last block ends with the scene");
        assert!(ai.enemy(3).is_none());
    }

    #[test]
    fn a_scene_with_no_or_garbage_ai_parses_to_nothing_without_panicking() {
        assert_eq!(SceneAi::parse(&[]), SceneAi::default());
        assert_eq!(SceneAi::parse(&vec![0xFF; 0x1E80]), SceneAi::default());
        // Offsets inside the table itself and past the end of the scene are ignored.
        let mut scene = vec![0u8; 0x1E80];
        scene[ENEMY_TABLE + 2..ENEMY_TABLE + 4].copy_from_slice(&0xFFF0u16.to_le_bytes());
        scene[ENEMY_TABLE + 4..ENEMY_TABLE + 6].copy_from_slice(&0x1E00u16.to_le_bytes());
        let ai = SceneAi::parse(&scene);
        assert!(ai.enemy(0).is_none(), "offset 0 points into the table");
        assert!(ai.enemy(1).is_none());
        assert!(ai.enemy(2).is_none(), "no room for a header");
    }
}
