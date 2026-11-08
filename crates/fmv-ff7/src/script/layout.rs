//! Container layout of the field script section (section 0): header, entity names, AKAO
//! offsets, per-entity script entry tables, script spans and the dialog table.
//!
//! Layout (offsets from the start of the section payload; FFRTT "FF7/Field/Script"
//! <https://wiki.ffrtt.ru/index.php/FF7/Field/Script>, mirrored in the ff7-flat-wiki):
//!
//! ```text
//! 0x00 u8  event data version (2 in retail)   0x01 u8 event version (5 in retail)
//! 0x02 u8  entity count E                     0x03 u8 model count
//! 0x04 u16 string (dialog) table offset       0x06 u16 AKAO block count A
//! 0x08 u16 field scale (9-bit fixed point)    0x0A 3 x u16 blank
//! 0x10 char[8] creator   0x18 char[8] name    0x20 char[8] x E  entity names
//! then u32 x A  AKAO block offsets,  then u16 x 32 x E  script entry points
//! then the scripts, then (at the string offset) u16 dialog count D, u16 x D pointers
//! (relative to the count word), the 0xFF-terminated strings, padding, and the AKAO blocks.
//! ```
//!
//! All offsets stored in the section (entry points, string offset, AKAO offsets) are relative
//! to the start of the payload, i.e. to the version byte.

use super::text::{decode_dialog, DecodedText};
use super::ScriptError;
use crate::util::{nul_padded_name, slice_at, u16_at, u32_at, u8_at};

/// Bytes of fixed header.
pub const HEADER_LEN: usize = 0x20;
/// Bytes of one entity name.
pub const ENTITY_NAME_LEN: usize = 8;
/// Script slots per entity (script 0 is the init script, script 1 the main loop).
pub const SCRIPTS_PER_ENTITY: usize = 32;
/// Index of the init script. Script 0 holds two scripts back to back: the *init* part up to its
/// first `RET` and the entity's *main* part after it (see [`crate::script::Phase`]).
pub const INIT_SCRIPT: usize = 0;
/// Entry slot of a model entity's talk script (`S1 - Talk` in Makou Reactor's
/// `GrpScript::scriptName`, <https://github.com/myst6re/makoureactor>): it runs when the player
/// presses the action button next to the entity.
pub const TALK_SCRIPT: usize = 1;
/// Entry slot of a model entity's contact script (`S2 - Contact`).
pub const CONTACT_SCRIPT: usize = 2;
/// Cap on dialog strings (the count is a `u16`).
pub const MAX_DIALOGS: usize = 65_535;

/// The fixed 0x20-byte header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptHeader {
    /// 2 in retail data (1 in the early demo).
    pub data_version: u8,
    /// 5 in retail data (3 in the early demo).
    pub event_version: u8,
    pub entity_count: u8,
    pub model_count: u8,
    /// Offset of the dialog table (which is also the end of the last script).
    pub string_offset: u16,
    pub akao_count: u16,
    /// Field scale, 512 = 1.0 (same value as the model loader's; see
    /// [`crate::field::ModelLoaderHeader`]).
    pub scale: u16,
    pub blank: [u16; 3],
    /// Author string (never displayed by the game).
    pub creator: String,
    /// Internal map name (never displayed by the game).
    pub name: String,
}

/// One entity (script group): its name and its 32 script entry points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityEntry {
    pub name: String,
    /// Entry points, section-relative; slot `i` is script `i`.
    pub entries: [u16; SCRIPTS_PER_ENTITY],
}

/// The byte range `[start, end)` of one script, section-relative. `end` is the next higher
/// entry point of any entity, or the dialog table for the last script, so the range may include
/// trailing padding after the script's final instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScriptSpan {
    pub start: usize,
    pub end: usize,
}

impl ScriptSpan {
    pub fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Location of one dialog string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DialogRef {
    pub index: usize,
    /// Section-relative offset of the first byte of the string.
    pub offset: usize,
    /// Bytes from `offset` through the terminating `0xFF` (or to the end of the section when
    /// the string is unterminated).
    pub len: usize,
    pub terminated: bool,
}

/// One AKAO (or tutorial) block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AkaoBlock {
    pub index: usize,
    pub offset: u32,
    /// Bytes up to the next block (or the end of the section for the last).
    pub len: usize,
    /// The first four bytes: `b"AKAO"` for sound data. Tutorial blocks (four fields) carry
    /// something else.
    pub tag: [u8; 4],
}

impl AkaoBlock {
    pub fn is_akao(&self) -> bool {
        &self.tag == b"AKAO"
    }
}

/// Everything about the section's layout except the opcodes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub header: ScriptHeader,
    pub entities: Vec<EntityEntry>,
    pub akao_offsets: Vec<u32>,
    /// Offset of the first byte after the entry tables (where the first script can start).
    pub code_start: usize,
    /// End of the scripts: the dialog table offset.
    pub code_end: usize,
    /// `spans[entity][slot]`; `None` when the entry point lies outside `code_start..code_end`.
    pub spans: Vec<[Option<ScriptSpan>; SCRIPTS_PER_ENTITY]>,
    /// Dialog strings (empty if the table could not be read, see `dialog_error`).
    pub dialogs: Vec<DialogRef>,
    pub dialog_error: Option<ScriptError>,
    /// Offset of the dialog count word.
    pub dialog_table: usize,
    pub akao_blocks: Vec<AkaoBlock>,
}

impl Layout {
    pub fn parse(data: &[u8]) -> Result<Self, ScriptError> {
        if data.len() < HEADER_LEN {
            return Err(ScriptError::TooShort { len: data.len() });
        }
        let truncated = |what: &'static str, at: usize, needed: usize| ScriptError::Truncated { what, at, needed, len: data.len() };
        let rd8 = |o: usize| u8_at(data, o).ok_or_else(|| truncated("header", o, 1));
        let rd16 = |o: usize| u16_at(data, o).ok_or_else(|| truncated("header", o, 2));
        let header = ScriptHeader {
            data_version: rd8(0)?,
            event_version: rd8(1)?,
            entity_count: rd8(2)?,
            model_count: rd8(3)?,
            string_offset: rd16(4)?,
            akao_count: rd16(6)?,
            scale: rd16(8)?,
            blank: [rd16(0x0A)?, rd16(0x0C)?, rd16(0x0E)?],
            creator: nul_padded_name(slice_at(data, 0x10, 8).ok_or_else(|| truncated("header", 0x10, 8))?),
            name: nul_padded_name(slice_at(data, 0x18, 8).ok_or_else(|| truncated("header", 0x18, 8))?),
        };

        let entity_count = usize::from(header.entity_count);
        let akao_count = usize::from(header.akao_count);

        // Entity names (E x 8 bytes), AKAO offsets (A x 4), entry tables (E x 64): the counts are
        // a u8 and a u16, so none of these products can overflow a usize.
        let names_at = HEADER_LEN;
        let akao_at = names_at + entity_count * ENTITY_NAME_LEN;
        let table_at = akao_at + akao_count * 4;
        let code_start = table_at + entity_count * SCRIPTS_PER_ENTITY * 2;
        if data.len() < code_start {
            return Err(truncated("entity and entry tables", HEADER_LEN, code_start - HEADER_LEN));
        }

        let mut entities = Vec::with_capacity(entity_count);
        for e in 0..entity_count {
            let name = nul_padded_name(
                slice_at(data, names_at + e * ENTITY_NAME_LEN, ENTITY_NAME_LEN).ok_or_else(|| truncated("entity names", names_at, entity_count * ENTITY_NAME_LEN))?,
            );
            let mut entries = [0u16; SCRIPTS_PER_ENTITY];
            for (slot, entry) in entries.iter_mut().enumerate() {
                let at = table_at + (e * SCRIPTS_PER_ENTITY + slot) * 2;
                *entry = u16_at(data, at).ok_or_else(|| truncated("script entry table", at, 2))?;
            }
            entities.push(EntityEntry { name, entries });
        }
        let mut akao_offsets = Vec::with_capacity(akao_count);
        for a in 0..akao_count {
            let at = akao_at + a * 4;
            akao_offsets.push(u32_at(data, at).ok_or_else(|| truncated("AKAO offsets", at, 4))?);
        }

        // Scripts run from code_start to the dialog table. A string offset outside that window
        // (damaged data) falls back to the end of the section so the scripts stay decodable.
        let dialog_table = usize::from(header.string_offset);
        let code_end = if dialog_table >= code_start && dialog_table <= data.len() { dialog_table } else { data.len() };

        // Entry points must lie inside the script area. (29 empty slots of `snw_w` point at the
        // dialog table, one byte past the last script; the game never runs them, so they have no
        // code here.)
        let in_area = |o: usize| o >= code_start && o < code_end;
        let mut starts: Vec<usize> =
            entities.iter().flat_map(|e| e.entries.iter().map(|&o| usize::from(o))).filter(|&o| in_area(o)).collect();
        starts.sort_unstable();
        starts.dedup();
        let spans = entities
            .iter()
            .map(|e| {
                let mut row = [None; SCRIPTS_PER_ENTITY];
                for (slot, span) in row.iter_mut().enumerate() {
                    let start = usize::from(e.entries.get(slot).copied().unwrap_or(0));
                    if !in_area(start) {
                        continue;
                    }
                    let next = starts.partition_point(|&s| s <= start);
                    let end = starts.get(next).copied().unwrap_or(code_end);
                    *span = Some(ScriptSpan { start, end });
                }
                row
            })
            .collect();

        let (dialogs, dialog_error) = match parse_dialog_table(data, dialog_table, &akao_offsets) {
            Ok(d) => (d, None),
            Err(e) => (Vec::new(), Some(e)),
        };
        let akao_blocks = parse_akao_blocks(data, &akao_offsets);

        Ok(Self { header, entities, akao_offsets, code_start, code_end, spans, dialogs, dialog_error, dialog_table, akao_blocks })
    }
}

/// Read the dialog table at `table`. A field without dialogs has no table at all: fewer than two
/// bytes remain after the scripts (`nivgate3`, `nivl_4`) or the first AKAO block starts there
/// (`las4_42`); both mean zero dialogs. A string is cut off at the next AKAO block, which in
/// two fields (`life`, `life2`) begins at the one listed "string" (so it has length zero and no
/// terminator).
fn parse_dialog_table(data: &[u8], table: usize, akao_offsets: &[u32]) -> Result<Vec<DialogRef>, ScriptError> {
    let bad = |why: &'static str| ScriptError::Dialogs(why);
    if table > data.len() {
        return Err(bad("dialog table offset lies outside the section"));
    }
    if data.len() - table < 2 || akao_offsets.iter().any(|&o| usize::try_from(o).ok() == Some(table)) {
        return Ok(Vec::new());
    }
    let count = usize::from(u16_at(data, table).ok_or(bad("dialog count lies outside the section"))?);
    if count > MAX_DIALOGS {
        return Err(bad("too many dialogs"));
    }
    // count <= 65_535 and table <= data.len() (u16_at succeeded), so these cannot overflow.
    let ptrs_at = table + 2;
    if slice_at(data, ptrs_at, count * 2).is_none() {
        return Err(bad("dialog pointer table is cut off by the end of the section"));
    }
    let mut out = Vec::with_capacity(count);
    for index in 0..count {
        let ptr = usize::from(u16_at(data, ptrs_at + index * 2).ok_or(bad("dialog pointer out of range"))?);
        let offset = table.checked_add(ptr).ok_or(bad("dialog pointer overflows"))?;
        let limit = akao_offsets
            .iter()
            .filter_map(|&o| usize::try_from(o).ok())
            .filter(|&o| o >= offset)
            .min()
            .unwrap_or(data.len())
            .min(data.len());
        let rest = data.get(offset..limit).unwrap_or(&[]);
        let DecodedText { consumed, terminated, .. } = decode_dialog(rest);
        out.push(DialogRef { index, offset, len: consumed, terminated });
    }
    Ok(out)
}

fn parse_akao_blocks(data: &[u8], offsets: &[u32]) -> Vec<AkaoBlock> {
    let mut sorted: Vec<usize> = offsets.iter().filter_map(|&o| usize::try_from(o).ok()).filter(|&o| o < data.len()).collect();
    sorted.sort_unstable();
    sorted.dedup();
    offsets
        .iter()
        .enumerate()
        .filter_map(|(index, &offset)| {
            let start = usize::try_from(offset).ok()?;
            if start >= data.len() {
                return None;
            }
            let next = sorted.partition_point(|&s| s <= start);
            let end = sorted.get(next).copied().unwrap_or(data.len());
            let mut tag = [0u8; 4];
            for (k, slot) in tag.iter_mut().enumerate() {
                *slot = u8_at(data, start + k).unwrap_or(0);
            }
            Some(AkaoBlock { index, offset, len: end.saturating_sub(start), tag })
        })
        .collect()
}
