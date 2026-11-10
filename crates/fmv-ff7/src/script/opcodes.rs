//! The field-script opcode table: mnemonic and byte layout of every opcode.
//!
//! Sources (layout facts only; no code copied):
//!
//! * FFRTT / Qhimm wiki "FF7/Field/Script/Opcodes" and its per-opcode pages
//!   <https://wiki.ffrtt.ru/index.php/FF7/Field/Script/Opcodes> (mirrored as static pages under
//!   <https://ff7-mods.github.io/ff7-flat-wiki/FF7/Field/Script/Opcodes.html>, for example
//!   `.../Opcodes/14_IFUB.html`): mnemonics, argument types, bank nibble pairing, jump bases
//!   (forward jumps count from the jump operand, backward jumps from the opcode byte), the
//!   `CHAR`, `SOLID` and `VISI` semantics.
//! * Makou Reactor's `src/core/field/Opcode.cpp` and `Opcode.h`
//!   <https://github.com/myst6re/makoureactor>: the total length of every opcode and the field
//!   order of each opcode's operands (the documentation pages leave many unspecified). A unit
//!   test below compares every layout's width with that length table.
//! * The owner's `flevel.lgp`: every script of all 702 fields decodes instruction by
//!   instruction exactly to its end, every jump lands on an instruction start (or on the first
//!   byte after its script), and no bank nibble that a layout leaves unassigned is ever
//!   non-zero (see the module docs of [`super`]). 222 of the 243 assigned opcodes occur in the
//!   data; the other 21 (`HMPMAX1`, `HMPMAX2`, `WROW`, `GWCOL`, `CMTRA`, `NOP`, `SCRLO`,
//!   `MINUS2!`, `DEC2!`, `BITXOR`, `XOR2`, `LBYTE`, `HBYTE`, `SETX`, `GETX`, `SEARCHX`, `SLDR2`,
//!   `PMJMP2`, `RTPAL2`, `MUSVM`, `CHMPH`) and the `SPECIAL` sub-opcodes `PNAME` and `GMSPD` rest
//!   on the documentation and Makou's table alone.
//!
//! A layout is the list of [`Fld`]s after the opcode byte. Bank bytes come first in almost every
//! opcode (`Banks(n)` reads `n` bytes, two nibbles each, high nibble first); a banked operand
//! names the nibble that governs it (nibble 0 = high nibble of the first bank byte). When a bank
//! byte serves a single operand its nibble is usually the low one (the pages write that as
//! `0 / B2`; the owned data confirm it for `INC`, `DEC`, `RANDOM`, `BGCLR`, `DIR`, `TALKR`, ...).
//! `WNUMB` currently uses a combined 32-bit operand governed by the high nibble.
//! `GOLDu`/`GOLDd` have two independently banked 16-bit halves (low, then high). The owned
//! scripts happen to use a literal high half; original-engine disassembly establishes the
//! second selector as meaningful too (`q-gears-reversing-data`, `opcodes_party.cpp`, 800D3BB4/800D3C44).


use super::decode::JumpDir;

/// Opcode byte of `SPECIAL` (a sub-opcode follows).
pub(crate) const SPECIAL: u8 = 0x0F;
/// Opcode byte of `KAWAI` (length byte and sub-opcode follow).
pub(crate) const KAWAI: u8 = 0x28;

/// One element of an opcode's operand layout (the bytes after the opcode byte, in order).
#[derive(Debug, Clone, Copy)]
pub(crate) enum Fld {
    /// `n` bytes of bank nibbles (high nibble of the first byte is nibble 0).
    Banks(u8),
    /// Banked operand: name, index of the governing bank nibble.
    Bu8(&'static str, u8),
    Bu16(&'static str, u8),
    Bs16(&'static str, u8),
    Bs32(&'static str, u8),
    U8(&'static str),
    U16(&'static str),
    S16(&'static str),
    S32(&'static str),
    /// One byte: 3-bit priority (high bits) and 5-bit script number (low bits).
    PrioScript,
    /// Relative jump operand of the given width in bytes (1 or 2).
    Jump(&'static str, JumpDir, u8),
}

impl Fld {
    /// Bytes this element occupies in the instruction.
    pub(crate) fn width(&self) -> usize {
        match *self {
            Fld::Banks(n) => usize::from(n),
            Fld::Bu8(..) | Fld::U8(_) | Fld::PrioScript => 1,
            Fld::Bu16(..) | Fld::Bs16(..) | Fld::U16(_) | Fld::S16(_) => 2,
            Fld::Bs32(..) | Fld::S32(_) => 4,
            Fld::Jump(_, _, w) => usize::from(w),
        }
    }
}

/// Bytes a layout occupies after the opcode (and sub-opcode) byte.
pub(crate) fn layout_width(fields: &[Fld]) -> usize {
    fields.iter().map(Fld::width).sum()
}

/// Mnemonic and layout of one opcode.
#[derive(Debug, Clone, Copy)]
pub(crate) struct OpInfo {
    pub mnemonic: &'static str,
    pub fields: &'static [Fld],
}

macro_rules! op {
    ($m:expr, [$($f:expr),* $(,)?]) => {
        Some(OpInfo { mnemonic: $m, fields: &[$($f),*] })
    };
}

/// Layout of opcode `op` (`None` for unassigned bytes). `SPECIAL` and `KAWAI` are listed with
/// their fixed prefix only (nothing after the opcode byte); the decoder handles their
/// sub-opcodes.
pub(crate) fn info(op: u8) -> Option<OpInfo> {
    use Fld::*;
    const FWD: JumpDir = JumpDir::Forward;
    const BACK: JumpDir = JumpDir::Backward;
    match op {
        // 0x00-0x0F: script control, party
        0x00 => op!("RET", []),
        0x01 => op!("REQ", [U8("entity"), PrioScript]),
        0x02 => op!("REQSW", [U8("entity"), PrioScript]),
        0x03 => op!("REQEW", [U8("entity"), PrioScript]),
        0x04 => op!("PREQ", [U8("party"), PrioScript]),
        0x05 => op!("PRQSW", [U8("party"), PrioScript]),
        0x06 => op!("PRQEW", [U8("party"), PrioScript]),
        0x07 => op!("RETTO", [PrioScript]),
        0x08 => op!("JOIN", [U8("speed")]),
        0x09 => op!(
            "SPLIT",
            [Banks(3), Bs16("xa", 0), Bs16("ya", 1), Bu8("da", 2), Bs16("xb", 3), Bs16("yb", 4), Bu8("db", 5), U8("speed")]
        ),
        0x0A => op!("SPTYE", [Banks(2), Bu8("character1", 0), Bu8("character2", 1), Bu8("character3", 2)]),
        0x0B => op!("GTPYE", [Banks(2), Bu8("var1", 0), Bu8("var2", 1), Bu8("var3", 2)]),
        0x0E => op!("DSKCG", [U8("disk")]),
        0x0F => op!("SPECIAL", []),
        // 0x10-0x19: jumps and conditionals
        0x10 => op!("JMPF", [Jump("jump", FWD, 1)]),
        0x11 => op!("JMPFL", [Jump("jump", FWD, 2)]),
        0x12 => op!("JMPB", [Jump("jump", BACK, 1)]),
        0x13 => op!("JMPBL", [Jump("jump", BACK, 2)]),
        0x14 => op!("IFUB", [Banks(1), Bu8("left", 0), Bu8("right", 1), U8("op"), Jump("jump", FWD, 1)]),
        0x15 => op!("IFUBL", [Banks(1), Bu8("left", 0), Bu8("right", 1), U8("op"), Jump("jump", FWD, 2)]),
        0x16 => op!("IFSW", [Banks(1), Bs16("left", 0), Bs16("right", 1), U8("op"), Jump("jump", FWD, 1)]),
        0x17 => op!("IFSWL", [Banks(1), Bs16("left", 0), Bs16("right", 1), U8("op"), Jump("jump", FWD, 2)]),
        0x18 => op!("IFUW", [Banks(1), Bu16("left", 0), Bu16("right", 1), U8("op"), Jump("jump", FWD, 1)]),
        0x19 => op!("IFUWL", [Banks(1), Bu16("left", 0), Bu16("right", 1), U8("op"), Jump("jump", FWD, 2)]),
        // 0x20-0x2F: modules, waits, background, windows
        0x20 => op!("MINIGAME", [U16("field"), S16("x"), S16("y"), U16("triangle"), U8("param"), U8("game")]),
        0x21 => op!("TUTOR", [U8("tutorial")]),
        0x22 => op!("BTMD2", [S32("mode")]),
        0x23 => op!("BTRLD", [Banks(1), Bu8("var", 1)]),
        0x24 => op!("WAIT", [U16("frames")]),
        0x25 => op!("NFADE", [Banks(2), U8("type"), Bu8("r", 0), Bu8("g", 1), Bu8("b", 2), Bu16("speed", 3)]),
        0x26 => op!("BLINK", [U8("closed")]),
        0x27 => op!("BGMOVIE", [U8("disabled")]),
        0x28 => op!("KAWAI", []),
        0x29 => op!("KAWIW", []),
        0x2A => op!("PMOVA", [U8("party")]),
        0x2B => op!("SLIP", [U8("disabled")]),
        0x2C => op!("BGPDH", [Banks(1), U8("layer"), Bs16("z", 1)]),
        0x2D => op!("BGSCR", [Banks(1), U8("layer"), Bs16("x", 0), Bs16("y", 1)]),
        0x2E => op!("WCLS", [U8("window")]),
        0x2F => op!("WSIZW", [U8("window"), S16("x"), S16("y"), U16("width"), U16("height")]),
        // 0x30-0x3F: input, window text, money
        0x30 => op!("IFKEY", [U16("keys"), Jump("jump", FWD, 1)]),
        0x31 => op!("IFKEYON", [U16("keys"), Jump("jump", FWD, 1)]),
        0x32 => op!("IFKEYOFF", [U16("keys"), Jump("jump", FWD, 1)]),
        0x33 => op!("UC", [U8("disabled")]),
        0x34 => op!("PDIRA", [U8("party")]),
        0x35 => op!("PTURA", [U8("party"), U8("speed"), U8("direction")]),
        0x36 => op!("WSPCL", [U8("window"), U8("type"), U8("margin_left"), U8("margin_top")]),
        0x37 => op!("WNUMB", [Banks(1), U8("window"), Bs32("value", 0), U8("digits")]),
        0x38 => op!("STTIM", [Banks(2), Bu8("hours", 0), Bu8("minutes", 1), Bu8("seconds", 2)]),
        0x39 => op!("GOLDu", [Banks(1), Bu16("amount_lo", 0), Bu16("amount_hi", 1)]),
        0x3A => op!("GOLDd", [Banks(1), Bu16("amount_lo", 0), Bu16("amount_hi", 1)]),
        0x3B => op!("CHGLD", [Banks(1), Bu8("var1", 0), Bu8("var2", 1)]),
        0x3C => op!("HMPMAX1", []),
        0x3D => op!("HMPMAX2", []),
        0x3E => op!("MHMMX", []),
        0x3F => op!("HMPMAX3", []),
        // 0x40-0x4F: messages and menus
        0x40 => op!("MESSAGE", [U8("window"), U8("dialog")]),
        0x41 => op!("MPARA", [Banks(1), U8("window"), U8("index"), Bu8("value", 1)]),
        0x42 => op!("MPRA2", [Banks(1), U8("window"), U8("index"), Bu16("value", 1)]),
        0x43 => op!("MPNAM", [U8("name")]),
        0x45 => op!("MPu", [Banks(1), Bu8("party", 0), Bu16("value", 1)]),
        0x47 => op!("MPd", [Banks(1), Bu8("party", 0), Bu16("value", 1)]),
        0x48 => op!("ASK", [Banks(1), U8("window"), U8("dialog"), U8("first"), U8("last"), Bu8("var", 1)]),
        0x49 => op!("MENU", [Banks(1), U8("type"), Bu8("param", 1)]),
        0x4A => op!("MENU2", [U8("disabled")]),
        0x4B => op!("BTLTB", [U8("table")]),
        0x4D => op!("HPu", [Banks(1), Bu8("party", 0), Bu16("value", 1)]),
        0x4F => op!("HPd", [Banks(1), Bu8("party", 0), Bu16("value", 1)]),
        // 0x50-0x5F: windows, items, materia
        0x50 => op!("WINDOW", [U8("window"), S16("x"), S16("y"), U16("width"), U16("height")]),
        0x51 => op!("WMOVE", [U8("window"), S16("dx"), S16("dy")]),
        0x52 => op!("WMODE", [U8("window"), U8("mode"), U8("prevent_close")]),
        0x53 => op!("WREST", [U8("window")]),
        0x54 => op!("WCLSE", [U8("window")]),
        0x55 => op!("WROW", [U8("window"), U8("rows")]),
        0x56 => op!("GWCOL", [Banks(2), Bu8("corner", 0), Bu8("var_r", 1), Bu8("var_g", 2), Bu8("var_b", 3)]),
        0x57 => op!("SWCOL", [Banks(2), Bu8("corner", 0), Bu8("r", 1), Bu8("g", 2), Bu8("b", 3)]),
        0x58 => op!("STITM", [Banks(1), Bu16("item", 0), Bu8("quantity", 1)]),
        0x59 => op!("DLITM", [Banks(1), Bu16("item", 0), Bu8("quantity", 1)]),
        0x5A => op!("CKITM", [Banks(1), Bu16("item", 0), Bu8("quantity", 1)]),
        0x5B => op!("SMTRA", [Banks(2), Bu8("materia", 0), Bu8("ap0", 1), Bu8("ap1", 2), Bu8("ap2", 3)]),
        0x5C => op!("DMTRA", [Banks(2), Bu8("materia", 0), Bu8("ap0", 1), Bu8("ap1", 2), Bu8("ap2", 3), U8("quantity")]),
        0x5D => op!(
            "CMTRA",
            [Banks(3), Bu8("ap0", 0), Bu8("ap1", 1), Bu8("ap2", 2), Bu8("ap3", 3), Bu8("materia", 4), Bu8("var_quantity", 5)]
        ),
        0x5E => op!(
            "SHAKE",
            [Banks(2), U8("type"), Bu8("x_amplitude", 0), Bu8("x_frames", 1), Bu8("y_amplitude", 2), Bu8("y_frames", 3)]
        ),
        0x5F => op!("NOP", []),
        // 0x60-0x6F: field change, scrolling, fades
        0x60 => op!("MAPJUMP", [U16("field"), S16("x"), S16("y"), S16("triangle"), U8("direction")]),
        0x61 => op!("SCRLO", [U8("unknown")]),
        0x62 => op!("SCRLC", [Banks(1), Bu16("speed", 1), U8("unknown")]),
        0x63 => op!("SCRLA", [Banks(1), Bu16("speed", 0), Bu8("entity", 1), U8("scroll_type")]),
        0x64 => op!("SCR2D", [Banks(1), Bs16("x", 0), Bs16("y", 1)]),
        0x65 => op!("SCRCC", []),
        0x66 => op!("SCR2DC", [Banks(2), Bs16("x", 0), Bs16("y", 1), Bu16("speed", 2)]),
        0x67 => op!("SCRLW", []),
        0x68 => op!("SCR2DL", [Banks(2), Bs16("x", 0), Bs16("y", 1), Bu16("speed", 2)]),
        0x69 => op!("MPDSP", [U8("unknown")]),
        0x6A => op!("VWOFT", [Banks(1), Bu16("unknown1", 0), Bu16("unknown2", 1), U8("enable")]),
        0x6B => op!("FADE", [Banks(2), Bu8("r", 0), Bu8("g", 1), Bu8("b", 2), U8("speed"), U8("type"), U8("adjust")]),
        0x6C => op!("FADEW", []),
        0x6D => op!("IDLCK", [U16("triangle"), U8("locked")]),
        0x6E => op!("LSTMP", [Banks(1), Bu8("var", 1)]),
        0x6F => op!("SCRLP", [Banks(1), Bu16("speed", 0), Bu8("party", 1), U8("scroll_type")]),
        // 0x70-0x7F: battle, party, saturating arithmetic
        0x70 => op!("BATTLE", [Banks(1), Bu16("battle", 1)]),
        0x71 => op!("BTLON", [U8("disabled")]),
        0x72 => op!("BTLMD", [U16("flags")]),
        0x73 => op!("PGTDR", [Banks(1), U8("party"), Bu8("var_direction", 1)]),
        0x74 => op!("GETPC", [Banks(1), U8("party"), Bu8("var_character", 1)]),
        0x75 => op!("PXYZI", [Banks(2), U8("party"), Bu8("var_x", 0), Bu8("var_y", 1), Bu8("var_z", 2), Bu8("var_triangle", 3)]),
        0x76 => op!("PLUS!", [Banks(1), Bu8("var", 0), Bu8("value", 1)]),
        0x77 => op!("PLUS2!", [Banks(1), Bu8("var", 0), Bs16("value", 1)]),
        0x78 => op!("MINUS!", [Banks(1), Bu8("var", 0), Bu8("value", 1)]),
        0x79 => op!("MINUS2!", [Banks(1), Bu8("var", 0), Bs16("value", 1)]),
        0x7A => op!("INC!", [Banks(1), Bu8("var", 1)]),
        0x7B => op!("INC2!", [Banks(1), Bu8("var", 1)]),
        0x7C => op!("DEC!", [Banks(1), Bu8("var", 1)]),
        0x7D => op!("DEC2!", [Banks(1), Bu8("var", 1)]),
        0x7E => op!("TLKON", [U8("disabled")]),
        0x7F => op!("RDMSD", [Banks(1), Bu8("value", 1)]),
        // 0x80-0x9F: variables and arithmetic
        0x80 => op!("SETBYTE", [Banks(1), Bu8("var", 0), Bu8("value", 1)]),
        0x81 => op!("SETWORD", [Banks(1), Bu8("var", 0), Bs16("value", 1)]),
        0x82 => op!("BITON", [Banks(1), Bu8("var", 0), Bu8("bit", 1)]),
        0x83 => op!("BITOFF", [Banks(1), Bu8("var", 0), Bu8("bit", 1)]),
        0x84 => op!("BITXOR", [Banks(1), Bu8("var", 0), Bu8("bit", 1)]),
        0x85 => op!("PLUS", [Banks(1), Bu8("var", 0), Bu8("value", 1)]),
        0x86 => op!("PLUS2", [Banks(1), Bu8("var", 0), Bs16("value", 1)]),
        0x87 => op!("MINUS", [Banks(1), Bu8("var", 0), Bu8("value", 1)]),
        0x88 => op!("MINUS2", [Banks(1), Bu8("var", 0), Bs16("value", 1)]),
        0x89 => op!("MUL", [Banks(1), Bu8("var", 0), Bu8("value", 1)]),
        0x8A => op!("MUL2", [Banks(1), Bu8("var", 0), Bs16("value", 1)]),
        0x8B => op!("DIV", [Banks(1), Bu8("var", 0), Bu8("value", 1)]),
        0x8C => op!("DIV2", [Banks(1), Bu8("var", 0), Bs16("value", 1)]),
        0x8D => op!("MOD", [Banks(1), Bu8("var", 0), Bu8("value", 1)]),
        0x8E => op!("MOD2", [Banks(1), Bu8("var", 0), Bs16("value", 1)]),
        0x8F => op!("AND", [Banks(1), Bu8("var", 0), Bu8("value", 1)]),
        0x90 => op!("AND2", [Banks(1), Bu8("var", 0), Bs16("value", 1)]),
        0x91 => op!("OR", [Banks(1), Bu8("var", 0), Bu8("value", 1)]),
        0x92 => op!("OR2", [Banks(1), Bu8("var", 0), Bs16("value", 1)]),
        0x93 => op!("XOR", [Banks(1), Bu8("var", 0), Bu8("value", 1)]),
        0x94 => op!("XOR2", [Banks(1), Bu8("var", 0), Bs16("value", 1)]),
        0x95 => op!("INC", [Banks(1), Bu8("var", 1)]),
        0x96 => op!("INC2", [Banks(1), Bu8("var", 1)]),
        0x97 => op!("DEC", [Banks(1), Bu8("var", 1)]),
        0x98 => op!("DEC2", [Banks(1), Bu8("var", 1)]),
        0x99 => op!("RANDOM", [Banks(1), Bu8("var", 1)]),
        0x9A => op!("LBYTE", [Banks(1), Bu8("var", 0), Bu8("value", 1)]),
        0x9B => op!("HBYTE", [Banks(1), Bu8("var", 0), Bu16("value", 1)]),
        0x9C => op!("2BYTE", [Banks(2), Bu8("var", 0), Bu8("low", 1), Bu8("high", 2)]),
        0x9D => op!("SETX", [Banks(2), Bu8("value", 0), Bu16("a", 1), Bu8("b", 2)]),
        0x9E => op!("GETX", [Banks(2), Bu8("value", 0), Bu16("a", 1), Bu8("var", 2)]),
        0x9F => op!(
            "SEARCHX",
            [Banks(3), Bu8("search_start", 0), Bu16("start", 1), Bu16("end", 2), Bu8("value", 3), Bu8("var_result", 4)]
        ),
        // 0xA0-0xBF: field objects
        0xA0 => op!("PC", [U8("character")]),
        0xA1 => op!("CHAR", [U8("char_id")]),
        0xA2 => op!("DFANM", [U8("animation"), U8("speed")]),
        0xA3 => op!("ANIME1", [U8("animation"), U8("speed")]),
        0xA4 => op!("VISI", [U8("visible")]),
        0xA5 => op!("XYZI", [Banks(2), Bs16("x", 0), Bs16("y", 1), Bs16("z", 2), Bu16("triangle", 3)]),
        0xA6 => op!("XYI", [Banks(2), Bs16("x", 0), Bs16("y", 1), Bu16("triangle", 2)]),
        0xA7 => op!("XYZ", [Banks(2), Bs16("x", 0), Bs16("y", 1), Bs16("z", 2)]),
        0xA8 => op!("MOVE", [Banks(1), Bs16("x", 0), Bs16("y", 1)]),
        0xA9 => op!("CMOVE", [Banks(1), Bs16("x", 0), Bs16("y", 1)]),
        0xAA => op!("MOVA", [U8("entity")]),
        0xAB => op!("TURA", [U8("entity"), U8("speed"), U8("direction")]),
        0xAC => op!("ANIMW", []),
        0xAD => op!("FMOVE", [Banks(1), Bs16("x", 0), Bs16("y", 1)]),
        0xAE => op!("ANIME2", [U8("animation"), U8("speed")]),
        0xAF => op!("ANIM!1", [U8("animation"), U8("speed")]),
        0xB0 => op!("CANIM1", [U8("animation"), U8("first"), U8("last"), U8("speed")]),
        0xB1 => op!("CANM!1", [U8("animation"), U8("first"), U8("last"), U8("speed")]),
        0xB2 => op!("MSPED", [Banks(1), Bu16("speed", 1)]),
        0xB3 => op!("DIR", [Banks(1), Bu8("direction", 1)]),
        0xB4 => op!("TURNGEN", [Banks(1), Bu8("direction", 1), U8("turns"), U8("speed"), U8("unknown")]),
        0xB5 => op!("TURN", [Banks(1), Bu8("direction", 1), U8("turns"), U8("speed"), U8("unknown")]),
        0xB6 => op!("DIRA", [U8("entity")]),
        0xB7 => op!("GETDIR", [Banks(1), U8("entity"), Bu8("var_direction", 1)]),
        0xB8 => op!("GETAXY", [Banks(1), U8("entity"), Bu8("var_x", 0), Bu8("var_y", 1)]),
        0xB9 => op!("GETAI", [Banks(1), U8("entity"), Bu8("var_triangle", 1)]),
        0xBA => op!("ANIM!2", [U8("animation"), U8("speed")]),
        0xBB => op!("CANIM2", [U8("animation"), U8("first"), U8("last"), U8("speed")]),
        0xBC => op!("CANM!2", [U8("animation"), U8("first"), U8("last"), U8("speed")]),
        0xBD => op!("ASPED", [Banks(1), Bu16("speed", 1)]),
        0xBF => op!("CC", [U8("entity")]),
        // 0xC0-0xCF: movement, solidity, party
        0xC0 => op!("JUMP", [Banks(2), Bs16("x", 0), Bs16("y", 1), Bu16("triangle", 2), Bs16("height", 3)]),
        0xC1 => op!("AXYZI", [Banks(2), U8("entity"), Bu8("var_x", 0), Bu8("var_y", 1), Bu8("var_z", 2), Bu8("var_triangle", 3)]),
        0xC2 => op!(
            "LADER",
            [Banks(2), Bs16("x", 0), Bs16("y", 1), Bs16("z", 2), Bu16("triangle", 3), U8("way"), U8("animation"), U8("direction"), U8("speed")]
        ),
        0xC3 => op!("OFST", [Banks(2), U8("move_type"), Bs16("x", 0), Bs16("y", 1), Bs16("z", 2), Bu16("speed", 3)]),
        0xC4 => op!("OFSTW", []),
        0xC5 => op!("TALKR", [Banks(1), Bu8("radius", 1)]),
        0xC6 => op!("SLIDR", [Banks(1), Bu8("radius", 1)]),
        0xC7 => op!("SOLID", [U8("solid")]),
        0xC8 => op!("PRTYP", [U8("character")]),
        0xC9 => op!("PRTYM", [U8("character")]),
        0xCA => op!("PRTYE", [U8("character1"), U8("character2"), U8("character3")]),
        0xCB => op!("IFPRTYQ", [U8("character"), Jump("jump", FWD, 1)]),
        0xCC => op!("IFMEMBQ", [U8("character"), Jump("jump", FWD, 1)]),
        0xCD => op!("MMBud", [U8("exists"), U8("character")]),
        0xCE => op!("MMBLK", [U8("character")]),
        0xCF => op!("MMBUK", [U8("character")]),
        // 0xD0-0xDF: lines, math, sound, misc
        0xD0 => op!("LINE", [S16("x1"), S16("y1"), S16("z1"), S16("x2"), S16("y2"), S16("z2")]),
        0xD1 => op!("LINON", [U8("enabled")]),
        0xD2 => op!("MPJPO", [U8("disabled")]),
        0xD3 => op!("SLINE", [Banks(3), Bs16("x1", 0), Bs16("y1", 1), Bs16("z1", 2), Bs16("x2", 3), Bs16("y2", 4), Bs16("z2", 5)]),
        0xD4 => op!("SIN", [Banks(2), Bs16("a", 0), Bs16("b", 1), Bs16("c", 2), Bu8("var", 3)]),
        0xD5 => op!("COS", [Banks(2), Bs16("a", 0), Bs16("b", 1), Bs16("c", 2), Bu8("var", 3)]),
        0xD6 => op!("TLKR2", [Banks(1), Bu16("radius", 1)]),
        0xD7 => op!("SLDR2", [Banks(1), Bu16("radius", 1)]),
        0xD8 => op!("PMJMP", [U16("field")]),
        0xD9 => op!("PMJMP2", []),
        0xDA => op!("AKAO2", [Banks(3), U8("op"), Bu16("p1", 0), Bu16("p2", 1), Bu16("p3", 2), Bu16("p4", 3), Bu16("p5", 5)]),
        0xDB => op!("FCFIX", [U8("disabled")]),
        0xDC => op!("CCANM", [U8("animation"), U8("speed"), U8("mode")]),
        0xDD => op!("ANIMB", []),
        0xDE => op!("TURNW", []),
        0xDF => op!(
            "MPPAL",
            [Banks(3), Bu8("pos_src", 0), Bu8("pos_dst", 1), Bu8("start", 2), Bu8("b", 3), Bu8("g", 4), Bu8("r", 5), U8("count")]
        ),
        // 0xE0-0xEF: background and palettes
        0xE0 => op!("BGON", [Banks(1), Bu8("group", 0), Bu8("frame", 1)]),
        0xE1 => op!("BGOFF", [Banks(1), Bu8("group", 0), Bu8("frame", 1)]),
        0xE2 => op!("BGROL", [Banks(1), Bu8("group", 1)]),
        0xE3 => op!("BGROL2", [Banks(1), Bu8("group", 1)]),
        0xE4 => op!("BGCLR", [Banks(1), Bu8("group", 1)]),
        0xE5 => op!("STPAL", [Banks(1), Bu8("palette", 0), Bu8("position", 1), U8("count")]),
        0xE6 => op!("LDPAL", [Banks(1), Bu8("position", 0), Bu8("palette", 1), U8("count")]),
        0xE7 => op!("CPPAL", [Banks(1), Bu8("pos_src", 0), Bu8("pos_dst", 1), U8("count")]),
        0xE8 => op!("RTPAL", [Banks(2), Bu8("pos_src", 0), Bu8("pos_dst", 1), Bu8("start", 2), Bu8("end", 3)]),
        0xE9 => op!(
            "ADPAL",
            [Banks(3), Bu8("pos_src", 0), Bu8("pos_dst", 1), Bu8("b", 2), Bu8("g", 3), Bu8("r", 4), U8("count")]
        ),
        0xEA => op!(
            "MPPAL2",
            [Banks(3), Bu8("pos_src", 0), Bu8("pos_dst", 1), Bu8("b", 2), Bu8("g", 3), Bu8("r", 4), U8("count")]
        ),
        0xEB => op!("STPLS", [U8("palette"), U8("pos_src"), U8("start"), U8("count")]),
        0xEC => op!("LDPLS", [U8("pos_src"), U8("palette"), U8("start"), U8("count")]),
        0xED => op!(
            "CPPAL2",
            [Banks(2), Bu8("tile_src", 0), Bu8("tile_dst", 1), Bu8("pos_src", 2), Bu8("pos_dst", 3), U8("count")]
        ),
        0xEE => op!(
            "RTPAL2",
            [Banks(2), Bu8("tile_src", 0), Bu8("tile_dst", 1), Bu8("pos_src", 2), Bu8("pos_dst", 3), U8("start")]
        ),
        0xEF => op!(
            "ADPAL2",
            [Banks(3), Bu8("tile_src", 0), Bu8("tile_dst", 1), Bu8("start", 2), Bu8("b", 3), Bu8("g", 4), Bu8("r", 5), U8("count")]
        ),
        // 0xF0-0xFF: music, sound, movies
        0xF0 => op!("MUSIC", [U8("id")]),
        0xF1 => op!("SOUND", [Banks(1), Bu16("id", 0), Bu8("pan", 1)]),
        0xF2 => op!("AKAO", [Banks(3), U8("op"), Bu8("p1", 0), Bu16("p2", 1), Bu16("p3", 2), Bu16("p4", 3), Bu16("p5", 5)]),
        0xF3 => op!("MUSVT", [U8("id")]),
        0xF4 => op!("MUSVM", [U8("id")]),
        0xF5 => op!("MULCK", [U8("disabled")]),
        0xF6 => op!("BMUSC", [U8("id")]),
        0xF7 => op!("CHMPH", [Banks(1), Bu8("var1", 0), Bu8("var2", 1)]),
        0xF8 => op!("PMVIE", [U8("movie")]),
        0xF9 => op!("MOVIE", []),
        0xFA => op!("MVIEF", [Banks(1), Bu8("var", 1)]),
        0xFB => op!("MVCAM", [U8("camera")]),
        0xFC => op!("FMUSC", [U8("id")]),
        0xFD => op!("CMUSC", [U8("music"), Banks(1), U8("op"), Bu16("param1", 0), Bu16("param2", 1)]),
        0xFE => op!("CHMST", [Banks(1), Bu8("var", 1)]),
        0xFF => op!("GAMEOVER", []),
        _ => None,
    }
}

/// Layout of the `SPECIAL` sub-opcode `sub` (the bytes after `0F <sub>`).
///
/// `PNAME` (`F6`) and `GMSPD` (`F7`) never occur in the owned data and the community pages do
/// not exist for them; their one argument byte follows Makou Reactor's length table (a
/// `fixedSize` of base + 1).
pub(crate) fn special_info(sub: u8) -> Option<(&'static str, &'static [Fld])> {
    use Fld::*;
    Some(match sub {
        0xF5 => ("ARROW", &[U8("disabled")]),
        0xF6 => ("PNAME", &[U8("arg")]),
        0xF7 => ("GMSPD", &[U8("arg")]),
        0xF8 => ("SMSPD", &[Banks(1), Bu8("speed", 1)]),
        0xF9 => ("FLMAT", &[]),
        0xFA => ("FLITM", &[]),
        0xFB => ("BTLCK", &[U8("lock")]),
        0xFC => ("MVLCK", &[U8("lock")]),
        0xFD => ("SPCNM", &[U8("character"), U8("text")]),
        0xFE => ("RSGLB", &[]),
        0xFF => ("CLITM", &[]),
        _ => return None,
    })
}

/// Name of a `KAWAI` sub-opcode (the wiki lists seven; the owned data also use `04`, `07`, `08`,
/// `09`, `0B` and `0C`, which have no documented meaning).
pub(crate) fn kawai_sub_name(sub: u8) -> &'static str {
    match sub {
        0x00 => "EYETX",
        0x01 => "TRNSP",
        0x02 => "AMBNT",
        0x06 => "LIGHT",
        0x0A => "SBOBJ",
        0x0D => "SHINE",
        0xFF => "RESET",
        _ => "?",
    }
}

/// Operand layout of the `KAWAI` sub-opcodes whose arguments are known (read from the start of
/// the payload when it is long enough).
pub(crate) fn kawai_sub_layout(sub: u8) -> &'static [Fld] {
    use Fld::*;
    match sub {
        0x00 => &[U8("eye1"), U8("eye2"), U8("mouth"), U8("object")],
        0x01 => &[U8("enable")],
        0x02 => &[U8("r1"), U8("r2"), U8("g1"), U8("g2"), U8("b1"), U8("b2"), U8("flags")],
        _ => &[],
    }
}

/// Whether `op` is a jump or conditional jump (it carries a jump target).
pub fn is_jump(op: u8) -> bool {
    matches!(op, 0x10..=0x19 | 0x30..=0x32 | 0xCB | 0xCC)
}

/// Whether control never falls through to the next instruction after `op`.
pub fn ends_flow(op: u8) -> bool {
    matches!(op, 0x00 | 0x07 | 0x10..=0x13 | 0xFF)
}

/// Mnemonic of an opcode byte (`None` for unassigned bytes).
pub fn mnemonic(op: u8) -> Option<&'static str> {
    info(op).map(|i| i.mnemonic)
}

/// `(opcode, mnemonic)` for every assigned opcode in numeric order.
pub fn opcode_name_table() -> Vec<(u8, &'static str)> {
    (0..=255u8).filter_map(|op| mnemonic(op).map(|m| (op, m))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Total instruction length of every opcode (opcode byte included) as used by Makou
    /// Reactor's `Opcode::length` table; `0` marks the bytes the community pages call unused.
    /// `SPECIAL` (0x0F) and `KAWAI` (0x28) list their fixed prefix only.
    #[rustfmt::skip]
    const REFERENCE_LENGTHS: [u8; 256] = [
        1, 3, 3, 3, 3, 3, 3, 2, 2, 15, 6, 6, 0, 0, 2, 2,
        2, 3, 2, 3, 6, 7, 8, 9, 8, 9, 0, 0, 0, 0, 0, 0,
        11, 2, 5, 3, 3, 9, 2, 2, 3, 1, 2, 2, 5, 7, 2, 10,
        4, 4, 4, 2, 2, 4, 5, 8, 6, 6, 6, 4, 1, 1, 1, 1,
        3, 5, 6, 2, 0, 5, 0, 5, 7, 4, 2, 2, 0, 5, 0, 5,
        10, 6, 4, 2, 2, 3, 7, 7, 5, 5, 5, 7, 8, 10, 8, 1,
        10, 2, 5, 6, 6, 1, 9, 1, 9, 2, 7, 9, 1, 4, 3, 6,
        4, 2, 3, 4, 4, 8, 4, 5, 4, 5, 3, 3, 3, 3, 2, 3,
        4, 5, 4, 4, 4, 4, 5, 4, 5, 4, 5, 4, 5, 4, 5, 4,
        5, 4, 5, 4, 5, 3, 3, 3, 3, 3, 4, 5, 6, 7, 7, 11,
        2, 2, 3, 3, 2, 11, 9, 9, 6, 6, 2, 4, 1, 6, 3, 3,
        5, 5, 4, 3, 6, 6, 2, 4, 5, 4, 3, 5, 5, 4, 0, 2,
        11, 8, 15, 12, 1, 3, 3, 2, 2, 2, 4, 3, 3, 3, 2, 2,
        13, 2, 2, 16, 10, 10, 4, 4, 3, 1, 15, 2, 4, 1, 1, 11,
        4, 4, 3, 3, 3, 5, 5, 5, 7, 10, 10, 5, 5, 8, 8, 11,
        2, 5, 14, 2, 2, 2, 2, 4, 2, 1, 3, 2, 2, 8, 3, 1,
    ];

    #[test]
    fn layouts_match_the_reference_lengths() {
        let mut checked = 0;
        for op in 0..=255u8 {
            let want = usize::from(REFERENCE_LENGTHS[usize::from(op)]);
            match info(op) {
                None => assert_eq!(want, 0, "opcode {op:#04X} has no layout but the reference gives it {want} bytes"),
                Some(i) => {
                    let fixed: usize = 1 + i.fields.iter().map(Fld::width).sum::<usize>();
                    // SPECIAL (2 = opcode + sub-opcode) and KAWAI (3 = opcode + length +
                    // sub-opcode) are completed by the decoder, so their table layouts are empty.
                    let table_len = match op {
                        SPECIAL => 2,
                        KAWAI => 3,
                        _ => fixed,
                    };
                    assert_eq!(table_len, want, "{} ({op:#04X}): layout is {table_len} bytes, reference says {want}", i.mnemonic);
                    checked += 1;
                }
            }
        }
        assert_eq!(checked, 243);
    }

    #[test]
    fn bank_nibbles_are_assigned_at_most_once_and_exist() {
        for op in 0..=255u8 {
            let Some(i) = info(op) else { continue };
            let mut nibbles = 0usize;
            let mut used = [false; 16];
            for f in i.fields {
                match *f {
                    Fld::Banks(n) => nibbles += usize::from(n) * 2,
                    Fld::Bu8(_, n) | Fld::Bu16(_, n) | Fld::Bs16(_, n) | Fld::Bs32(_, n) => {
                        let n = usize::from(n);
                        assert!(n < nibbles, "{} uses nibble {n} but only {nibbles} exist before it", i.mnemonic);
                        assert!(!used[n], "{} assigns nibble {n} twice", i.mnemonic);
                        used[n] = true;
                    }
                    _ => {}
                }
            }
        }
    }

    #[test]
    fn special_sub_opcode_lengths_match_the_reference() {
        // base 2 bytes + extra: ARROW, PNAME, GMSPD, BTLCK, MVLCK one; SMSPD, SPCNM two; the rest none
        let extra = |sub: u8| -> usize { special_info(sub).map_or(0, |(_, f)| f.iter().map(Fld::width).sum()) };
        for sub in [0xF5, 0xF6, 0xF7, 0xFB, 0xFC] {
            assert_eq!(extra(sub), 1, "sub {sub:#04X}");
        }
        for sub in [0xF8, 0xFD] {
            assert_eq!(extra(sub), 2, "sub {sub:#04X}");
        }
        for sub in [0xF9, 0xFA, 0xFE, 0xFF] {
            assert_eq!(extra(sub), 0, "sub {sub:#04X}");
        }
        assert!(special_info(0x00).is_none());
    }

    #[test]
    fn mnemonics_are_unique_and_jumps_have_a_jump_operand() {
        let table = opcode_name_table();
        assert_eq!(table.len(), 243);
        let mut names: Vec<&str> = table.iter().map(|(_, m)| *m).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 243);
        for (op, m) in table {
            let has_jump = info(op).is_some_and(|i| i.fields.iter().any(|f| matches!(f, Fld::Jump(..))));
            assert_eq!(has_jump, is_jump(op), "{m} ({op:#04X})");
        }
    }

    #[test]
    fn kawai_layouts_fit_their_known_payloads() {
        let size = |sub: u8| kawai_sub_layout(sub).iter().map(Fld::width).sum::<usize>();
        assert_eq!((size(0), size(1), size(2), size(6)), (4, 1, 7, 0));
    }
}
