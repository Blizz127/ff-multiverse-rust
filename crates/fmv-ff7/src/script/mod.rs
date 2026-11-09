//! Field script (section 0 of a PC field file): container, opcode decoder, disassembler,
//! dialog text and static analysis of the entities' init scripts.
//!
//! **Verified on owned FF7 Steam Edition (en) on 2026-11-08; other editions are unverified.**
//! Measured facts (counts and offsets only; no game bytes and no game text are stored here):
//!
//! * Container: all 702 field scripts parse. Version bytes are `2`/`5` everywhere, the three
//!   blank header words are zero, the header scale equals the model loader's scale, and the string
//!   offset is exactly the end of the last script. The 10 534 entities have 337 088 entry slots;
//!   most unused slots share one entry (288 972 slots point at a script an earlier slot already
//!   uses). 29 slots of one entity of `snw_w` point at the dialog table, one byte past the last
//!   script, and are treated as having no code.
//! * Decoder: the 48 087 distinct scripts (343 587 instructions) all decode instruction by
//!   instruction **exactly to their end, with zero desyncs**. 45 509 end in `RET`, 1 589 in a
//!   backward jump (an idle loop), 9 in `RETTO` and 7 in a forward jump; 947 end without any
//!   terminator and fall into a `RET` that follows, and 26 more fall into other code (the scripts
//!   of the next entry): real quirks of the data, not decode errors. All 40 940 jumps land on an
//!   instruction start (40 321) or on the first byte after their script (619, 612 of them on a
//!   `RET`). 222 of the 243 assigned opcodes occur; no bank nibble that a layout leaves
//!   unassigned is ever non-zero in the 89 opcodes that use variables.
//! * Script 0 is two scripts in one (below): 10 516 of the 10 533 distinct script 0s have both
//!   parts; in one of them the first `RET` sits inside a conditional branch.
//! * Dialogs: 56 048 strings; three fields (`las4_42`, `nivgate3`, `nivl_4`) have no dialog table
//!   at all (read as zero dialogs); in every other table the first pointer is exactly
//!   `2 + 2 * count`; three strings have no `0xFF` terminator (`life`, `life2`, `mds7pb_2`: they
//!   run into the AKAO block that follows). 1 342 AKAO blocks; 22 blocks in six fields
//!   (`junair2`, `junone7`, `junpb_2`, `mds7pb_1`, `mds7pb_2`, `mds7_w2`) carry something else
//!   (tutorials).
//! * Models: the model loader walks exactly to the end of its section in all 702 fields, the
//!   script's model count equals the loader's count, the number of entities with a `CHAR` equals
//!   it too (5 454 in total), and the `CHAR` operand equals the record index in every case.
//! * Players: 38 fields bind no playable character, the rest bind one to ten (character ids
//!   `0..=10`; every field with a party carries one entity per character that may appear).
//! * Placements: 3 014 `XYZI`/`XYI`/`XYZ` instructions in script 0, 1 340 of them conditional
//!   and 507 in the main part; of the literal ones naming a triangle, 2 978 lie inside it (height
//!   within 20 units of the triangle plane, 6 off by more than 8), 15 outside and one names a
//!   triangle past the mesh. `md1stin`, `md1_1` and `mds7st1` have no placement off its triangle.
//! * Exits: all 1 036 gateways resolve to maplist names (978 target a field in the archive);
//!   1 801 `MAPJUMP`s with literal operands resolve too (1 754 into the archive), and 1 383 of
//!   them arrive inside the target triangle named by their fourth operand (315 are `(0, 0, 0)`
//!   placeholders; the other 56 land off the named triangle, many of them warps out of the debug
//!   `blackbg*` fields).
//!
//! # Sources
//!
//! * Section layout: FFRTT "FF7/Field/Script" <https://wiki.ffrtt.ru/index.php/FF7/Field/Script>
//!   (mirrored at <https://ff7-mods.github.io/ff7-flat-wiki/FF7/Field/Script.html>).
//! * Opcodes: FFRTT "FF7/Field/Script/Opcodes" and its per-opcode pages
//!   <https://wiki.ffrtt.ru/index.php/FF7/Field/Script/Opcodes>; lengths and operand order
//!   cross-checked against Makou Reactor's `Opcode.cpp`/`Opcode.h` (see [`opcodes`] docs).
//! * Text: FFRTT "FF7/FF Text" <https://wiki.ffrtt.ru/index.php/FF7/FF_Text> and
//!   "FF7/Field/DialogWindow" (see the `text` module).
//! * Model loader: FFRTT "FF7/Field/Model_Loader"
//!   <https://wiki.ffrtt.ru/index.php/FF7/Field/Model_Loader>.
//! * Slot and group conventions (`Init`/`Main`, `Talk`, `Contact`): Makou Reactor's
//!   `GrpScript.cpp` <https://github.com/myst6re/makoureactor>.
//!
//! Only layout *facts* were taken from the community sources; no code was copied.
//!
//! # What the section holds
//!
//! A 32-byte header, one 8-byte name and 32 `u16` entry points per entity, then the scripts,
//! the dialog table and the AKAO blocks ([`Layout`]). A script's length is not stored: it runs
//! to the next higher entry point of any entity ([`ScriptSpan`]). Unused slots share an entry
//! that points at a `RET`.
//!
//! **Script 0 is an init part and a main part.** Slot 0's code runs from its entry point to the
//! first `RET` (executed once at field load) and the bytes after that `RET` are the entity's
//! *main* part (an idle loop `JMPB 0`, or logic that places and shows the entity depending on
//! story variables). Makou Reactor stores them as two scripts (`S0 - Init`, `S0 - Main`) and
//! joins them on export. Slot 1 of a model entity is its *talk* script and slot 2 its *contact*
//! script ([`TALK_SCRIPT`], [`CONTACT_SCRIPT`]); line entities (`LINE` in script 0) use slots
//! 1..=6 for `[OK]`, `Move`, `Move`, `Go`, `Go 1x`, `Go away`.
//!
//! **`CHAR` does not pick a model; order does.** The game gives the model-loader records, in
//! order, to the entities whose script 0 runs `CHAR`. In the owned data the operand equals that
//! order everywhere, so [`EntityInit::model_index`] is the order and [`EntityInit::char_id`] the
//! operand.
//!
//! **Scripts need not end in `RET`.** A script may fall into the next entry's code, and a jump
//! may land on the first byte after its script; the decoder accepts both, the analysis stops a
//! walk at the span end.
//!
//! # Using it
//!
//! ```ignore
//! let field = FieldFile::parse(decompressed)?;
//! let script = FieldScript::from_field(&field)?;
//! let models = parse_model_list(field.section(2)?)?;
//! for e in script.entity_inits(Some(&models)) {
//!     if let (Some(m), Some(p)) = (&e.model_ref, e.placement()) { /* hrc, x, y, z, triangle */ }
//! }
//! let listing = script.disassemble(entity, INIT_SCRIPT)?;          // Vec<Instruction>
//! let exits = resolve_gateways(&field.triggers()?, &maplist);       // names the target fields
//! ```
//!
//! # Safety against untrusted input
//!
//! Like the rest of the crate: no indexing, unchecked arithmetic or panics on untrusted bytes;
//! every table is bounds-checked and every walk ends at the end of its slice. Unit tests feed
//! every opcode every truncation and filler, thousands of random instruction streams, and
//! mutated and random whole sections through every public entry point with overflow checks on
//! (`FMV_SCRIPT_FUZZ=200000 cargo test -p fmv-ff7 --lib script::tests_fuzz` runs 800 000
//! sections; the last such run found no panic).
//!
//! # Known gaps
//!
//! * Only literal operands are evaluated; operands that name variables are reported as such and
//!   conditions are never evaluated, so story-dependent placements are listed with a
//!   `conditional` flag rather than resolved.
//! * Line entities, and which `REQ` chain turns a line crossing into a `MAPJUMP`, are not
//!   resolved; [`FieldScript::map_jumps`] lists every `MAPJUMP` with literal operands instead.
//! * 21 opcodes and the `SPECIAL` sub-opcodes `PNAME` and `GMSPD` never occur in the owned data,
//!   so their layouts rest on the documentation and Makou's table alone (lengths are tested
//!   against that table, not against data). Six `KAWAI` sub-opcodes used by the data
//!   (`04`, `07`, `08`, `09`, `0B`, `0C`) have no documented meaning and keep a raw payload.
//! * The nibble-to-operand pairing of rarely used opcodes follows the order the pages list;
//!   the data only prove that no unassigned nibble is ever used.
//! * Text decoding is verified for the English table; control codes with arguments follow the
//!   Dialog Window page, and a few bytes without a glyph appear as `{XX}`.

mod analysis;
mod decode;
mod layout;
mod maplist;
mod models;
mod exits;
mod opcodes;
mod text;

#[cfg(test)]
mod synth;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_decode;
#[cfg(test)]
mod tests_fuzz;

pub use analysis::*;
pub use decode::*;
pub use layout::*;
pub use maplist::*;
pub use models::*;
pub use exits::{
    CompareOp, CompareWidth, ExitGuard, ExitLine, ExitTrigger, GuardTest, GuardValue, KeyTest, ScriptExit, ScriptRef, LINE_SLOTS,
    MAX_CALL_DEPTH, MAX_SCRIPT_EXITS,
};
pub use opcodes::{is_jump, ends_flow, mnemonic, opcode_name_table};
pub use text::*;

use crate::field::{FieldError, FieldFile};
use thiserror::Error;

/// Errors from the script module.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ScriptError {
    #[error("script section is {len} bytes, shorter than the {HEADER_LEN}-byte header")]
    TooShort { len: usize },
    #[error("script section: {what} cut off at offset {at} (needs {needed} bytes, section has {len})")]
    Truncated { what: &'static str, at: usize, needed: usize, len: usize },
    #[error("dialog table: {0}")]
    Dialogs(&'static str),
    #[error("maplist: {0}")]
    Maplist(&'static str),
    #[error("maplist entry could not be read: {0}")]
    MaplistRead(String),
    #[error("field file: {0}")]
    Field(String),
    #[error("entity {0} does not exist")]
    NoSuchEntity(usize),
    #[error("script slot {0} is out of range (an entity has {SCRIPTS_PER_ENTITY})")]
    NoSuchScript(usize),
    #[error("entity {entity} script {script} has no code: its entry point lies outside the script area")]
    NoCode { entity: usize, script: usize },
    #[error("entity {entity} script {script}: {source}")]
    Decode { entity: usize, script: usize, source: DecodeError },
}

impl From<FieldError> for ScriptError {
    fn from(e: FieldError) -> Self {
        Self::Field(e.to_string())
    }
}

/// A parsed field script section, borrowing the section's bytes.
#[derive(Debug, Clone)]
pub struct FieldScript<'a> {
    data: &'a [u8],
    layout: Layout,
}

impl<'a> FieldScript<'a> {
    /// Parse the section payload (section 0 of a field file).
    pub fn parse(section: &'a [u8]) -> Result<Self, ScriptError> {
        Ok(Self { data: section, layout: Layout::parse(section)? })
    }

    /// Parse section 0 of `field`.
    pub fn from_field(field: &'a FieldFile) -> Result<Self, ScriptError> {
        let section = field.section(crate::field::section::SCRIPT)?;
        Self::parse(section)
    }

    pub fn header(&self) -> &ScriptHeader {
        &self.layout.header
    }

    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// The whole section payload.
    pub fn bytes(&self) -> &'a [u8] {
        self.data
    }

    pub fn entities(&self) -> &[EntityEntry] {
        &self.layout.entities
    }

    pub fn entity_count(&self) -> usize {
        self.layout.entities.len()
    }

    pub fn entity_name(&self, entity: usize) -> Option<&str> {
        self.layout.entities.get(entity).map(|e| e.name.as_str())
    }

    /// Index of the first entity called `name`.
    pub fn entity_by_name(&self, name: &str) -> Option<usize> {
        self.layout.entities.iter().position(|e| e.name == name)
    }

    /// Byte range of script `script` (0..32) of `entity`, or `None` if either index is out of
    /// range or the entry point lies outside the script area.
    pub fn span(&self, entity: usize, script: usize) -> Option<ScriptSpan> {
        self.layout.spans.get(entity)?.get(script).copied().flatten()
    }

    /// Bytes of one script (including any padding after its last instruction).
    pub fn script_bytes(&self, entity: usize, script: usize) -> Option<&'a [u8]> {
        let span = self.span(entity, script)?;
        self.data.get(span.start..span.end)
    }

    /// Walk a script as far as it decodes. Errors only for a bad `entity`/`script` index or an
    /// entry point outside the script area; a decode failure is in [`Disassembly::error`].
    pub fn disassemble_partial(&self, entity: usize, script: usize) -> Result<Disassembly, ScriptError> {
        if entity >= self.entity_count() {
            return Err(ScriptError::NoSuchEntity(entity));
        }
        if script >= SCRIPTS_PER_ENTITY {
            return Err(ScriptError::NoSuchScript(script));
        }
        let span = self.span(entity, script).ok_or(ScriptError::NoCode { entity, script })?;
        let code = self.data.get(span.start..span.end).unwrap_or(&[]);
        Ok(disassemble_slice(code, span.start))
    }

    /// Disassemble script `script` of `entity` instruction by instruction. Any byte that cannot
    /// be decoded is an error (so a returned list always covers the whole script span).
    pub fn disassemble(&self, entity: usize, script: usize) -> Result<Vec<Instruction>, ScriptError> {
        let d = self.disassemble_partial(entity, script)?;
        match d.error {
            None => Ok(d.instructions),
            Some(source) => Err(ScriptError::Decode { entity, script, source }),
        }
    }

    /// Dialog strings (locations only; see [`FieldScript::dialog_text`]).
    pub fn dialogs(&self) -> &[DialogRef] {
        &self.layout.dialogs
    }

    /// Why the dialog table could not be read, if it could not.
    pub fn dialog_error(&self) -> Option<&ScriptError> {
        self.layout.dialog_error.as_ref()
    }

    /// Decode dialog `index` to readable text.
    pub fn dialog_text(&self, index: usize) -> Option<DecodedText> {
        let d = self.layout.dialogs.get(index)?;
        Some(decode_dialog(self.data.get(d.offset..d.offset.checked_add(d.len)?)?))
    }

    pub fn akao_blocks(&self) -> &[AkaoBlock] {
        &self.layout.akao_blocks
    }
}
