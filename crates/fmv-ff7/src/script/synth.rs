//! Test-only builder of synthetic script sections (never real game data).

use super::layout::{ENTITY_NAME_LEN, HEADER_LEN, SCRIPTS_PER_ENTITY};

/// Description of one synthetic entity.
#[derive(Clone, Default)]
pub struct SynthEntity {
    pub name: String,
    /// `(slot, code)`; slots not listed point at their own single `RET`.
    pub scripts: Vec<(usize, Vec<u8>)>,
}

#[derive(Clone, Default)]
pub struct SynthScript {
    pub entities: Vec<SynthEntity>,
    /// Raw dialog strings (the builder appends nothing: include the 0xFF yourself).
    pub dialogs: Vec<Vec<u8>>,
    /// Raw AKAO / tutorial blocks.
    pub akao: Vec<Vec<u8>>,
    pub model_count: u8,
    pub scale: u16,
}

impl SynthScript {
    pub fn entity(mut self, name: &str, scripts: Vec<(usize, Vec<u8>)>) -> Self {
        self.entities.push(SynthEntity { name: name.to_string(), scripts });
        self
    }

    /// Serialise: header, names, AKAO offsets, entry tables, scripts, dialogs, padding, AKAO.
    pub fn build(&self) -> Vec<u8> {
        let e = self.entities.len();
        let a = self.akao.len();
        let code_start = HEADER_LEN + e * ENTITY_NAME_LEN + a * 4 + e * SCRIPTS_PER_ENTITY * 2;

        // lay out scripts, giving every unlisted slot its own RET
        let mut code: Vec<u8> = Vec::new();
        let mut entries: Vec<[u16; SCRIPTS_PER_ENTITY]> = Vec::new();
        for ent in &self.entities {
            let mut row = [0u16; SCRIPTS_PER_ENTITY];
            for (slot, entry) in row.iter_mut().enumerate() {
                let body = ent.scripts.iter().find(|(s, _)| *s == slot).map(|(_, c)| c.clone()).unwrap_or_else(|| vec![0x00]);
                *entry = u16::try_from(code_start + code.len()).unwrap();
                code.extend_from_slice(&body);
            }
            entries.push(row);
        }
        let string_offset = code_start + code.len();

        // dialog table
        let mut dialog_block: Vec<u8> = Vec::new();
        dialog_block.extend_from_slice(&u16::try_from(self.dialogs.len()).unwrap().to_le_bytes());
        let mut ptr = 2 + 2 * self.dialogs.len();
        for d in &self.dialogs {
            dialog_block.extend_from_slice(&u16::try_from(ptr).unwrap().to_le_bytes());
            ptr += d.len();
        }
        for d in &self.dialogs {
            dialog_block.extend_from_slice(d);
        }

        // AKAO blocks after the dialogs, padded to 4 bytes
        let mut akao_start = string_offset + dialog_block.len();
        while akao_start & 3 != 0 {
            akao_start += 1;
        }
        let mut akao_offsets = Vec::new();
        let mut akao_bytes = Vec::new();
        for b in &self.akao {
            akao_offsets.push(u32::try_from(akao_start + akao_bytes.len()).unwrap());
            akao_bytes.extend_from_slice(b);
        }

        let mut out = vec![0u8; HEADER_LEN];
        out[0] = 2;
        out[1] = 5;
        out[2] = u8::try_from(e).unwrap();
        out[3] = self.model_count;
        out[4..6].copy_from_slice(&u16::try_from(string_offset).unwrap().to_le_bytes());
        out[6..8].copy_from_slice(&u16::try_from(a).unwrap().to_le_bytes());
        out[8..10].copy_from_slice(&self.scale.to_le_bytes());
        out[0x10..0x18].copy_from_slice(b"synth\0\0\0");
        out[0x18..0x20].copy_from_slice(b"fixture\0");
        for ent in &self.entities {
            let mut n = [0u8; ENTITY_NAME_LEN];
            let bytes = ent.name.as_bytes();
            n[..bytes.len().min(ENTITY_NAME_LEN)].copy_from_slice(&bytes[..bytes.len().min(ENTITY_NAME_LEN)]);
            out.extend_from_slice(&n);
        }
        for o in &akao_offsets {
            out.extend_from_slice(&o.to_le_bytes());
        }
        for row in &entries {
            for v in row {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        out.extend_from_slice(&code);
        out.extend_from_slice(&dialog_block);
        while out.len() < akao_start {
            out.push(0);
        }
        out.extend_from_slice(&akao_bytes);
        out
    }
}

/// Encode printable ASCII as FF7 dialog bytes and terminate with 0xFF.
pub fn dialog(s: &str) -> Vec<u8> {
    let mut v: Vec<u8> = s.bytes().map(|b| b - 0x20).collect();
    v.push(0xFF);
    v
}
